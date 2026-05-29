use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use clap::{Parser, Subcommand};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::flag;

type Identity = (String, String);
type IdentityList = Vec<Identity>;

#[derive(Parser)]
#[command(
    name = "ral",
    version,
    about = "Cross-agent messaging for CLI AI agents"
)]
struct Cli {
    /// Data directory. Defaults to $RALLY_HOME, installed ~/.agents/skills/ral, or ~/.config/rally-rs.
    #[arg(long, global = true, env = "RALLY_HOME")]
    home: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize the message database.
    InitDb,
    /// Send a message to an agent.
    Send {
        team: String,
        from: String,
        to: String,
        message: String,
    },
    /// Show unread messages and mark them read.
    Inbox {
        team: String,
        agent: String,
        #[arg(long)]
        quiet: bool,
    },
    /// Show message history.
    History {
        team: String,
        agent: Option<String>,
        #[arg(default_value_t = 20)]
        limit: u32,
    },
    /// Join a team as an agent from a project.
    Join {
        team: String,
        agent: String,
        agent_type: String,
        project_path: PathBuf,
    },
    /// Leave a team completely.
    Leave { team: String, agent: String },
    /// Rename a team and migrate its messages.
    RenameTeam { old_team: String, new_team: String },
    /// Rename an agent in a team and migrate messages.
    Rename {
        team: String,
        old_agent: String,
        new_agent: String,
    },
    /// List team members.
    Team { team: String },
    /// Resolve the registered identity for a project and agent type.
    Whoami {
        project_path: PathBuf,
        agent_type: String,
    },
    /// List registered team/agent pairs for a project and agent type.
    Identities {
        project_path: PathBuf,
        agent_type: String,
    },
    /// Remove registrations for a project and agent type.
    Reset {
        project_path: PathBuf,
        agent_type: String,
        agent: Option<String>,
    },
    /// Manage simple YAML configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Manage automatic delivery modes and watcher processes.
    Delivery {
        #[command(subcommand)]
        command: DeliveryCommand,
    },
    /// Legacy alias for delivery set turn/off.
    Hook {
        #[command(subcommand)]
        command: HookCommand,
    },
    /// Check inbox for hook-based turn delivery.
    CheckInbox {
        agent_type: String,
        project_path: PathBuf,
    },
    /// Stream incoming messages for monitor delivery.
    Watch {
        session_id: String,
        project_path: PathBuf,
        agent_type: String,
        active_name: Option<String>,
    },
    /// Emit a monitor launch directive for a session.
    SessionStart {
        agent_type: String,
        project_path: PathBuf,
    },
    /// Stop the watcher for a session.
    SessionEnd {
        agent_type: String,
        project_path: PathBuf,
    },
    /// Print the bundled Agent Skill.
    #[command(visible_alias = "skills")]
    Skill {
        #[arg(long = "cmd", default_value = "ral")]
        cmd_name: String,
    },
    /// Install the skill, wrappers, and agent command assets.
    Install {
        #[arg(long = "cmd", default_value = "ral")]
        cmd_name: String,
        #[arg(long)]
        update: bool,
    },
    /// Remove installed skill assets.
    Uninstall {
        #[arg(long, short = 'y')]
        yes: bool,
        #[arg(long)]
        keep_data: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    Get {
        key: String,
        default: Option<String>,
    },
    Set {
        key: String,
        value: String,
    },
    Show,
}

#[derive(Subcommand)]
enum DeliveryCommand {
    Set {
        mode: String,
        agent_type: String,
        project_path: PathBuf,
    },
    Status {
        agent_type: Option<String>,
        project_path: Option<PathBuf>,
    },
    Stop,
    Restart {
        agent_type: Option<String>,
        project_path: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum HookCommand {
    On {
        agent_type: String,
        project_path: PathBuf,
    },
    Off {
        agent_type: String,
        project_path: PathBuf,
    },
}

#[derive(Clone)]
struct Store {
    root: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct TeamConfig {
    name: String,
    agents: BTreeMap<String, AgentConfig>,
    created_at: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct AgentConfig {
    registrations: Vec<Registration>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Registration {
    #[serde(rename = "type")]
    agent_type: String,
    project: String,
}

#[derive(Debug)]
struct Message {
    from: String,
    to: String,
    body: String,
    created_at: String,
    unread: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let store = Store::new(cli.home)?;

    match cli.command {
        Commands::InitDb => {
            store.init_db()?;
            println!("DB initialized: {}", store.db_path().display());
        }
        Commands::Send {
            team,
            from,
            to,
            message,
        } => send(&store, &team, &from, &to, &message)?,
        Commands::Inbox { team, agent, quiet } => inbox(&store, &team, &agent, quiet)?,
        Commands::History { team, agent, limit } => {
            history(&store, &team, agent.as_deref(), limit)?
        }
        Commands::Join {
            team,
            agent,
            agent_type,
            project_path,
        } => join(&store, &team, &agent, &agent_type, &project_path)?,
        Commands::Leave { team, agent } => leave(&store, &team, &agent)?,
        Commands::RenameTeam { old_team, new_team } => rename_team(&store, &old_team, &new_team)?,
        Commands::Rename {
            team,
            old_agent,
            new_agent,
        } => rename_agent(&store, &team, &old_agent, &new_agent)?,
        Commands::Team { team } => list_team(&store, &team)?,
        Commands::Whoami {
            project_path,
            agent_type,
        } => whoami(&store, &project_path, &agent_type)?,
        Commands::Identities {
            project_path,
            agent_type,
        } => {
            for (team, agent) in identities(&store, &project_path, &agent_type)? {
                println!("{team}\t{agent}");
            }
        }
        Commands::Reset {
            project_path,
            agent_type,
            agent,
        } => reset(&store, &project_path, &agent_type, agent.as_deref())?,
        Commands::Config { command } => match command {
            ConfigCommand::Get { key, default } => {
                println!("{}", config_get(&store, &key, default.as_deref())?);
            }
            ConfigCommand::Set { key, value } => {
                config_set(&store, &key, &value)?;
                println!("Set {key} = {value}");
            }
            ConfigCommand::Show => {
                let config = ensure_default_config(&store)?;
                print!("{}", fs::read_to_string(config)?);
            }
        },
        Commands::Delivery { command } => match command {
            DeliveryCommand::Set {
                mode,
                agent_type,
                project_path,
            } => delivery_set(&store, &mode, &agent_type, &project_path)?,
            DeliveryCommand::Status {
                agent_type,
                project_path,
            } => delivery_status(&store, agent_type.as_deref(), project_path.as_deref())?,
            DeliveryCommand::Stop => delivery_stop(&store)?,
            DeliveryCommand::Restart {
                agent_type,
                project_path,
            } => delivery_restart(&store, agent_type.as_deref(), project_path.as_deref())?,
        },
        Commands::Hook { command } => match command {
            HookCommand::On {
                agent_type,
                project_path,
            } => {
                eprintln!(
                    "ral: hook is deprecated; use 'delivery set <mode>' or '/ral mode <mode>' instead."
                );
                delivery_set(&store, "turn", &agent_type, &project_path)?
            }
            HookCommand::Off {
                agent_type,
                project_path,
            } => {
                eprintln!(
                    "ral: hook is deprecated; use 'delivery set <mode>' or '/ral mode <mode>' instead."
                );
                delivery_set(&store, "off", &agent_type, &project_path)?
            }
        },
        Commands::CheckInbox {
            agent_type,
            project_path,
        } => check_inbox(&store, &agent_type, &project_path)?,
        Commands::Watch {
            session_id,
            project_path,
            agent_type,
            active_name,
        } => watch(
            &store,
            &session_id,
            &project_path,
            &agent_type,
            active_name.as_deref(),
        )?,
        Commands::SessionStart {
            agent_type,
            project_path,
        } => session_start(&store, &agent_type, &project_path)?,
        Commands::SessionEnd {
            agent_type,
            project_path,
        } => session_end(&store, &agent_type, &project_path)?,
        Commands::Skill { cmd_name } => print_skill(&cmd_name),
        Commands::Install { cmd_name, update } => install(&cmd_name, update)?,
        Commands::Uninstall { yes, keep_data } => uninstall(yes, keep_data)?,
    }

    Ok(())
}

impl Store {
    fn new(home: Option<PathBuf>) -> Result<Self> {
        let root = match home {
            Some(path) => path,
            None => match env::var_os("RALLY_HOME") {
                Some(path) => PathBuf::from(path),
                None => default_home()?,
            },
        };
        Ok(Self { root })
    }

    fn db_dir(&self) -> PathBuf {
        self.root.join("db")
    }

    fn db_path(&self) -> PathBuf {
        self.db_dir().join("messages.db")
    }

    fn teams_dir(&self) -> PathBuf {
        self.root.join("teams")
    }

    fn config_path(&self) -> PathBuf {
        self.db_dir().join("config.yaml")
    }

    fn run_dir(&self) -> PathBuf {
        self.root.join("run")
    }

    fn scripts_dir(&self) -> PathBuf {
        self.root.join("scripts")
    }

    fn init_db(&self) -> Result<Connection> {
        fs::create_dir_all(self.db_dir())?;
        let conn = Connection::open(self.db_path())?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS messages (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              team TEXT NOT NULL,
              from_agent TEXT NOT NULL,
              to_agent TEXT NOT NULL,
              body TEXT NOT NULL,
              created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
              read_at TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_unread
              ON messages(team, to_agent, read_at) WHERE read_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_history
              ON messages(team, created_at DESC);
            "#,
        )?;
        Ok(conn)
    }

    fn open_db_if_exists(&self) -> Result<Option<Connection>> {
        if self.db_path().exists() {
            Ok(Some(Connection::open(self.db_path())?))
        } else {
            Ok(None)
        }
    }

    fn team_path(&self, team: &str) -> PathBuf {
        self.teams_dir().join(team)
    }

    fn team_config_path(&self, team: &str) -> PathBuf {
        self.team_path(team).join("config.json")
    }
}

fn default_home() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    let installed = home.join(".agents").join("skills").join("ral");
    if installed.join(".rally-rs").exists() {
        return Ok(installed);
    }
    Ok(home.join(".config").join("rally-rs"))
}

fn send(store: &Store, team: &str, from: &str, to: &str, body: &str) -> Result<()> {
    let conn = store.init_db()?;
    conn.execute(
        "INSERT INTO messages (team, from_agent, to_agent, body) VALUES (?1, ?2, ?3, ?4)",
        params![team, from, to, body],
    )?;
    println!("Sent to {to} in team {team}");
    Ok(())
}

fn inbox(store: &Store, team: &str, agent: &str, quiet: bool) -> Result<()> {
    let Some(conn) = store.open_db_if_exists()? else {
        if !quiet {
            println!("No messages (DB not initialized)");
        }
        return Ok(());
    };

    let unread = query_messages(
        &conn,
        "SELECT from_agent, to_agent, body, created_at, read_at IS NULL
         FROM messages
         WHERE team = ?1 AND to_agent = ?2 AND read_at IS NULL
         ORDER BY created_at ASC",
        params![team, agent],
    )?;

    if unread.is_empty() {
        if !quiet {
            println!("No new messages.");
        }
        return Ok(());
    }

    println!("{} new message(s):", unread.len());
    println!();
    for msg in &unread {
        println!(
            "  [{}] {}: {}",
            msg.created_at,
            msg.from,
            display_body(&msg.body)
        );
    }
    println!();

    conn.execute(
        "UPDATE messages
         SET read_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
         WHERE team = ?1 AND to_agent = ?2 AND read_at IS NULL",
        params![team, agent],
    )?;
    Ok(())
}

fn history(store: &Store, team: &str, agent: Option<&str>, limit: u32) -> Result<()> {
    let Some(conn) = store.open_db_if_exists()? else {
        println!("No messages (DB not initialized)");
        return Ok(());
    };

    let mut messages = if let Some(agent) = agent {
        query_messages(
            &conn,
            "SELECT from_agent, to_agent, body, created_at, read_at IS NULL
             FROM messages
             WHERE team = ?1 AND (from_agent = ?2 OR to_agent = ?2)
             ORDER BY created_at DESC
             LIMIT ?3",
            params![team, agent, limit],
        )?
    } else {
        query_messages(
            &conn,
            "SELECT from_agent, to_agent, body, created_at, read_at IS NULL
             FROM messages
             WHERE team = ?1
             ORDER BY created_at DESC
             LIMIT ?2",
            params![team, limit],
        )?
    };

    if messages.is_empty() {
        println!("No message history.");
        return Ok(());
    }

    messages.reverse();
    for msg in messages {
        let status = if msg.unread { "●" } else { "○" };
        println!(
            "  {status} [{}] {} → {}: {}",
            msg.created_at,
            msg.from,
            msg.to,
            display_body(&msg.body)
        );
    }
    Ok(())
}

fn query_messages<P>(conn: &Connection, sql: &str, params: P) -> Result<Vec<Message>>
where
    P: rusqlite::Params,
{
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params, |row| {
        Ok(Message {
            from: row.get(0)?,
            to: row.get(1)?,
            body: row.get(2)?,
            created_at: row.get(3)?,
            unread: row.get(4)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn display_body(body: &str) -> String {
    body.replace('\n', "\\n").replace('\t', "\\t")
}

fn join(
    store: &Store,
    team: &str,
    agent: &str,
    agent_type: &str,
    project_path: &Path,
) -> Result<()> {
    validate_agent_type(agent_type)?;
    fs::create_dir_all(store.team_path(team))?;
    let path = store.team_config_path(team);
    let mut config = if path.exists() {
        read_team_config(&path)?
    } else {
        println!("Created team: {team}");
        TeamConfig {
            name: team.to_owned(),
            agents: BTreeMap::new(),
            created_at: Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        }
    };

    let registration = Registration {
        agent_type: agent_type.to_owned(),
        project: normalize_project_path(project_path),
    };
    let entry = config
        .agents
        .entry(agent.to_owned())
        .or_insert_with(|| AgentConfig {
            registrations: Vec::new(),
        });
    if !entry.registrations.contains(&registration) {
        entry.registrations.push(registration);
    }
    write_team_config(&path, &config)?;
    println!("Joined team {team} as {agent}");
    Ok(())
}

fn leave(store: &Store, team: &str, agent: &str) -> Result<()> {
    let path = store.team_config_path(team);
    if !path.exists() {
        bail!("Team not found: {team}");
    }
    let mut config = read_team_config(&path)?;
    if config.agents.remove(agent).is_none() {
        bail!("Agent {agent} not in team {team}");
    }
    if config.agents.is_empty() {
        fs::remove_file(&path)?;
        let _ = fs::remove_dir(store.team_path(team));
        println!("Left team {team} (team removed — no members left)");
    } else {
        write_team_config(&path, &config)?;
        println!("Left team {team}");
    }
    Ok(())
}

fn rename_team(store: &Store, old_team: &str, new_team: &str) -> Result<()> {
    if old_team == new_team {
        bail!("Old and new team names are the same: {old_team}");
    }
    let old_dir = store.team_path(old_team);
    let new_dir = store.team_path(new_team);
    if !old_dir.exists() {
        bail!("Team not found: {old_team}");
    }
    if new_dir.exists() {
        bail!("Team already exists: {new_team}");
    }
    fs::rename(&old_dir, &new_dir)?;

    let config_path = store.team_config_path(new_team);
    if config_path.exists() {
        let mut config = read_team_config(&config_path)?;
        config.name = new_team.to_owned();
        write_team_config(&config_path, &config)?;
    }

    if let Some(conn) = store.open_db_if_exists()? {
        conn.execute(
            "UPDATE messages SET team = ?1 WHERE team = ?2",
            params![new_team, old_team],
        )?;
    }
    println!("Renamed team {old_team} → {new_team}");
    println!();
    println!("Note: existing sessions may have cached the old team name.");
    Ok(())
}

fn rename_agent(store: &Store, team: &str, old_agent: &str, new_agent: &str) -> Result<()> {
    let path = store.team_config_path(team);
    if !path.exists() {
        bail!("Team not found: {team}");
    }
    let mut config = read_team_config(&path)?;
    if !config.agents.contains_key(old_agent) {
        bail!("Agent {old_agent} not in team {team}");
    }
    if config.agents.contains_key(new_agent) {
        bail!("Agent {new_agent} already exists in team {team}");
    }
    let agent = config.agents.remove(old_agent).expect("checked above");
    config.agents.insert(new_agent.to_owned(), agent);
    write_team_config(&path, &config)?;

    if let Some(conn) = store.open_db_if_exists()? {
        conn.execute(
            "UPDATE messages SET from_agent = ?1 WHERE team = ?2 AND from_agent = ?3",
            params![new_agent, team, old_agent],
        )?;
        conn.execute(
            "UPDATE messages SET to_agent = ?1 WHERE team = ?2 AND to_agent = ?3",
            params![new_agent, team, old_agent],
        )?;
    }
    println!("Renamed {old_agent} → {new_agent} in team {team}");
    Ok(())
}

fn list_team(store: &Store, team: &str) -> Result<()> {
    let path = store.team_config_path(team);
    if !path.exists() {
        bail!("Team not found: {team}");
    }
    let config = read_team_config(&path)?;
    println!("Team: {team}");
    println!();
    for (name, agent) in &config.agents {
        let types = agent
            .registrations
            .iter()
            .map(|r| r.agent_type.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(",");
        let latest_project = agent
            .registrations
            .last()
            .map(|r| r.project.as_str())
            .unwrap_or("?");
        if agent.registrations.len() > 1 {
            println!(
                "  {name} ({types}) — {latest_project} (+{} more)",
                agent.registrations.len() - 1
            );
        } else {
            println!("  {name} ({types}) — {latest_project}");
        }
    }
    println!();
    println!("{} member(s)", config.agents.len());
    Ok(())
}

fn whoami(store: &Store, project_path: &Path, agent_type: &str) -> Result<()> {
    let exact = identities(store, project_path, agent_type)?;
    let (suggested, all_teams) = suggested_identities(store, agent_type)?;

    if exact.is_empty() && suggested.is_empty() {
        println!(
            "not_joined=true available_teams={}",
            comma_or_none(all_teams.into_iter())
        );
        return Ok(());
    }

    if exact.is_empty() {
        let agents = suggested
            .iter()
            .map(|(_, agent)| agent.as_str())
            .collect::<BTreeSet<_>>();
        let teams = suggested
            .iter()
            .map(|(team, _)| team.as_str())
            .collect::<BTreeSet<_>>();
        println!(
            "suggest=true agents={} teams={} type={} project={} available_teams={}",
            comma_or_none(agents.into_iter()),
            comma_or_none(teams.into_iter()),
            agent_type,
            normalize_project_path(project_path),
            comma_or_none(all_teams.into_iter())
        );
        return Ok(());
    }

    let agents = exact
        .iter()
        .map(|(_, agent)| agent.as_str())
        .collect::<BTreeSet<_>>();
    let teams = exact
        .iter()
        .map(|(team, _)| team.as_str())
        .collect::<BTreeSet<_>>();
    let prefix = if agents.len() == 1 {
        format!("agent={}", comma_or_none(agents.into_iter()))
    } else {
        format!("multiple=true agents={}", comma_or_none(agents.into_iter()))
    };
    println!(
        "{prefix} teams={} type={} project={}",
        comma_or_none(teams.into_iter()),
        agent_type,
        normalize_project_path(project_path)
    );
    Ok(())
}

fn identities(store: &Store, project_path: &Path, agent_type: &str) -> Result<IdentityList> {
    let project = normalize_project_path(project_path);
    let mut out = Vec::new();
    for (team, config) in team_configs(store)? {
        for (agent, agent_config) in config.agents {
            if agent_config
                .registrations
                .iter()
                .any(|r| r.project == project && r.agent_type == agent_type)
            {
                out.push((team.clone(), agent));
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn suggested_identities(
    store: &Store,
    agent_type: &str,
) -> Result<(IdentityList, BTreeSet<String>)> {
    let mut suggested = Vec::new();
    let mut all_teams = BTreeSet::new();
    for (team, config) in team_configs(store)? {
        all_teams.insert(team.clone());
        for (agent, agent_config) in config.agents {
            if agent_config
                .registrations
                .iter()
                .any(|r| r.agent_type == agent_type)
            {
                suggested.push((team.clone(), agent));
            }
        }
    }
    suggested.sort();
    suggested.dedup();
    Ok((suggested, all_teams))
}

fn reset(
    store: &Store,
    project_path: &Path,
    agent_type: &str,
    target_agent: Option<&str>,
) -> Result<()> {
    let target_agent = match target_agent {
        Some(agent) => agent.to_owned(),
        None => {
            let exact = identities(store, project_path, agent_type)?;
            let agents = exact
                .iter()
                .map(|(_, agent)| agent.as_str())
                .collect::<BTreeSet<_>>();
            match agents.len() {
                0 => bail!("No registered identity found for this project/type."),
                1 => agents.into_iter().next().unwrap().to_owned(),
                _ => bail!(
                    "Multiple identities match this project/type. Pass an agent_id explicitly."
                ),
            }
        }
    };

    if !store.teams_dir().exists() {
        println!("No team registrations found.");
        return Ok(());
    }

    let project = normalize_project_path(project_path);
    let mut removed = 0usize;
    let mut touched_teams = 0usize;

    for entry in fs::read_dir(store.teams_dir())? {
        let entry = entry?;
        let config_path = entry.path().join("config.json");
        if !config_path.exists() {
            continue;
        }
        let team_name = entry.file_name().to_string_lossy().to_string();
        let mut config = read_team_config(&config_path)?;
        let Some(agent_config) = config.agents.get_mut(&target_agent) else {
            continue;
        };

        let before = agent_config.registrations.len();
        agent_config
            .registrations
            .retain(|r| !(r.project == project && r.agent_type == agent_type));
        let count = before - agent_config.registrations.len();
        if count == 0 {
            continue;
        }

        if agent_config.registrations.is_empty() {
            config.agents.remove(&target_agent);
        }
        if config.agents.is_empty() {
            fs::remove_file(&config_path)?;
            let _ = fs::remove_dir(entry.path());
        } else {
            write_team_config(&config_path, &config)?;
        }
        removed += count;
        touched_teams += 1;
        println!("Cleared {count} registration(s) for {target_agent} from {team_name}");
    }

    if removed == 0 {
        println!("No registrations removed.");
    } else {
        println!(
            "Reset complete: removed {removed} registration(s) across {touched_teams} team(s)"
        );
    }
    Ok(())
}

fn delivery_set(store: &Store, mode: &str, agent_type: &str, project_path: &Path) -> Result<()> {
    match mode {
        "monitor" | "turn" | "both" | "off" => {}
        _ => bail!("Unknown mode: {mode} (use monitor|turn|both|off)"),
    }
    validate_agent_type(agent_type)?;
    apply_delivery_settings(store, agent_type, project_path, mode)?;
    println!(
        "Delivery mode set to '{mode}' for {} ({agent_type})",
        project_path.display()
    );
    match mode {
        "monitor" | "both" => {
            println!("Future sessions: SessionStart hook will auto-launch the watcher.");
            emit_monitor_directive(store, agent_type, project_path)?;
        }
        "turn" => {
            println!("Future sessions: Stop hook will check inbox between turns.");
            let _ = kill_all_watchers(store)?;
            emit_stop_directive();
        }
        "off" => {
            println!("Future sessions: no automatic delivery.");
            let _ = kill_all_watchers(store)?;
            emit_stop_directive();
        }
        _ => unreachable!(),
    }
    Ok(())
}

fn delivery_status(
    store: &Store,
    agent_type: Option<&str>,
    project_path: Option<&Path>,
) -> Result<()> {
    if let (Some(agent_type), Some(project_path)) = (agent_type, project_path) {
        let mode = delivery_mode(store, agent_type, project_path)?;
        println!("mode: {mode}");
        if !matches!(agent_type, "gemini" | "antigravity") {
            let file = hooks_file(agent_type, project_path)?;
            if file.exists() {
                let settings = read_json_file(&file)?;
                println!("settings hooks file: {}", file.display());
                println!(
                    "  SessionStart entries: {}",
                    hook_event_len(&settings, "SessionStart")
                );
                println!(
                    "  SessionEnd entries:   {}",
                    hook_event_len(&settings, "SessionEnd")
                );
                println!(
                    "  Stop entries:         {}",
                    hook_event_len(&settings, "Stop")
                );
            }
        }
    }
    let (alive, stale) = watcher_counts(store)?;
    if alive > 0 || stale > 0 {
        println!("watch processes: {alive} alive, {stale} stale pidfiles");
    }
    Ok(())
}

fn delivery_stop(store: &Store) -> Result<()> {
    let killed = kill_all_watchers(store)?;
    println!("Killed {killed} watch process(es).");
    emit_stop_directive();
    Ok(())
}

fn delivery_restart(
    store: &Store,
    agent_type: Option<&str>,
    project_path: Option<&Path>,
) -> Result<()> {
    let killed = kill_all_watchers(store)?;
    println!("Killed {killed} watch process(es).");
    emit_stop_directive();
    if let (Some(agent_type), Some(project_path)) = (agent_type, project_path) {
        emit_monitor_directive(store, agent_type, project_path)?;
    } else {
        println!();
        println!("To relaunch in this session, pass <type> <project_path>:");
        println!("  ral delivery restart claude-code /path/to/project");
    }
    Ok(())
}

fn check_inbox(store: &Store, agent_type: &str, project_path: &Path) -> Result<()> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    if input.contains("\"stop_hook_active\"") && input.contains("true") {
        return Ok(());
    }
    if let Some(session_id) = json_string_field(&input, "session_id")
        && watcher_alive_for_session(store, &session_id)?
    {
        return Ok(());
    }

    let pairs = identities(store, project_path, agent_type)?;
    if pairs.is_empty() {
        return Ok(());
    }

    let marker_agent = pairs
        .iter()
        .map(|(_, agent)| agent.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .next()
        .unwrap_or("agent")
        .to_owned();
    let marker = store.db_dir().join(format!(".lastcheck-{marker_agent}"));
    if marker.exists() {
        let last = fs::metadata(&marker)?
            .modified()
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let elapsed = SystemTime::now()
            .duration_since(last)
            .unwrap_or(Duration::from_secs(0));
        let interval = config_get(store, "delivery.turn.check_interval", Some(""))
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| config_get(store, "hook.check_interval", Some("60")).ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(60);
        if elapsed < Duration::from_secs(interval) {
            if agent_type == "codex" {
                println!(
                    "{}",
                    json!({"continue": true, "systemMessage": "ral: check skipped (cooldown)"})
                );
            }
            return Ok(());
        }
    }
    fs::create_dir_all(store.db_dir())?;
    fs::write(&marker, b"")?;

    let Some(conn) = store.open_db_if_exists()? else {
        return Ok(());
    };
    let mut output = String::new();
    for (team, agent) in pairs {
        let unread = query_messages(
            &conn,
            "SELECT from_agent, to_agent, body, created_at, read_at IS NULL
             FROM messages
             WHERE team = ?1 AND to_agent = ?2 AND read_at IS NULL
             ORDER BY created_at ASC",
            params![team, agent],
        )?;
        if unread.is_empty() {
            continue;
        }
        output.push_str(&format!("{} new message(s) in {team}:\n", unread.len()));
        for msg in &unread {
            output.push_str(&format!(
                "  [{}] {}: {}\n",
                msg.created_at,
                msg.from,
                display_body(&msg.body)
            ));
        }
        output.push('\n');
        conn.execute(
            "UPDATE messages
             SET read_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
             WHERE team = ?1 AND to_agent = ?2 AND read_at IS NULL",
            params![team, agent],
        )?;
    }

    if output.is_empty() {
        if agent_type == "codex" {
            println!(
                "{}",
                json!({"continue": true, "systemMessage": "ral: no new messages"})
            );
        }
    } else {
        println!("{}", json!({"decision": "block", "reason": output}));
    }
    Ok(())
}

fn watch(
    store: &Store,
    session_id: &str,
    project_path: &Path,
    agent_type: &str,
    active_name: Option<&str>,
) -> Result<()> {
    let mut pairs = identities(store, project_path, agent_type)?;
    if let Some(active_name) = active_name {
        pairs.retain(|(_, agent)| agent == active_name);
    }
    if pairs.is_empty() {
        if let Some(active_name) = active_name {
            println!(
                "ral watch: no registration for agent '{active_name}' in {} ({agent_type}); nothing to do",
                project_path.display()
            );
        } else {
            println!(
                "ral watch: no joined teams for {} ({agent_type}); nothing to do",
                project_path.display()
            );
        }
        return Ok(());
    }

    fs::create_dir_all(store.run_dir())?;
    let pidfile = watcher_pidfile(store, session_id);
    fs::write(&pidfile, process::id().to_string())?;
    let cleanup = PidfileCleanup(pidfile.clone());

    let term = Arc::new(AtomicBool::new(false));
    for sig in [SIGTERM, SIGINT, SIGHUP] {
        flag::register(sig, Arc::clone(&term))?;
    }

    let pair_set = pairs.into_iter().collect::<BTreeSet<_>>();
    let mut last = max_message_id(store, &pair_set)?;
    let interval = env::var("RAL_WATCH_INTERVAL")
        .or_else(|_| env::var("AGMSG_WATCH_INTERVAL"))
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .or_else(|| {
            config_get(store, "delivery.monitor.poll_interval", Some("5"))
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(5);

    while !term.load(Ordering::Relaxed) {
        if let Some(conn) = store.open_db_if_exists()? {
            let mut stmt = conn.prepare(
                "SELECT id, created_at, team, from_agent, to_agent, body
                 FROM messages WHERE id > ?1 ORDER BY id",
            )?;
            let rows = stmt.query_map(params![last], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?;
            for row in rows {
                let (id, ts, team, from, to, body) = row?;
                if pair_set.contains(&(team.clone(), to.clone())) {
                    println!(
                        "{ts} | {team} | {from} → {to} | {}",
                        body.replace('\r', "").replace('\n', "\\n")
                    );
                    io::stdout().flush()?;
                }
                last = last.max(id);
            }
        }
        for _ in 0..interval.max(1) * 10 {
            if term.load(Ordering::Relaxed) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    drop(cleanup);
    Ok(())
}

struct PidfileCleanup(PathBuf);

impl Drop for PidfileCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn session_start(store: &Store, agent_type: &str, project_path: &Path) -> Result<()> {
    let pairs = identities(store, project_path, agent_type)?;
    if pairs.is_empty() {
        return Ok(());
    }
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let session_id = json_string_field(&input, "session_id")
        .unwrap_or_else(|| format!("unknown-{}", process::id()));
    cleanup_stale_watchers(store)?;
    emit_monitor_directive_with_session(store, agent_type, project_path, &session_id);
    Ok(())
}

fn session_end(store: &Store, _agent_type: &str, _project_path: &Path) -> Result<()> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let Some(session_id) = json_string_field(&input, "session_id") else {
        return Ok(());
    };
    kill_watcher_for_session(store, &session_id)?;
    Ok(())
}

fn apply_delivery_settings(
    store: &Store,
    agent_type: &str,
    project_path: &Path,
    mode: &str,
) -> Result<()> {
    if matches!(agent_type, "gemini" | "antigravity") {
        let rule_file = hooks_file(agent_type, project_path)?;
        let _ = fs::remove_file(&rule_file);
        if matches!(mode, "turn" | "both" | "monitor") {
            if let Some(parent) = rule_file.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(
                rule_file,
                format!(
                    "# ral Integration Rule\n\n## PostToolUse\nAfter each tool call, check the ral inbox.\n- Command: '{}' --home '{}' check-inbox '{}' '{}'\n",
                    ral_command(),
                    store.root.display(),
                    agent_type,
                    project_path.display()
                ),
            )?;
        }
        return Ok(());
    }

    let hooks_file = hooks_file(agent_type, project_path)?;
    if let Some(parent) = hooks_file.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut settings = read_json_file(&hooks_file).unwrap_or_else(|_| json!({}));
    for event in ["SessionStart", "SessionEnd", "Stop"] {
        strip_owned_event(&mut settings, event, store);
    }
    match mode {
        "monitor" => {
            add_hook_event(
                &mut settings,
                "SessionStart",
                hook_command(store, "session-start", agent_type, project_path),
            );
            add_hook_event(
                &mut settings,
                "SessionEnd",
                hook_command(store, "session-end", agent_type, project_path),
            );
        }
        "turn" => add_hook_event(
            &mut settings,
            "Stop",
            hook_command(store, "check-inbox", agent_type, project_path),
        ),
        "both" => {
            add_hook_event(
                &mut settings,
                "SessionStart",
                hook_command(store, "session-start", agent_type, project_path),
            );
            add_hook_event(
                &mut settings,
                "SessionEnd",
                hook_command(store, "session-end", agent_type, project_path),
            );
            add_hook_event(
                &mut settings,
                "Stop",
                hook_command(store, "check-inbox", agent_type, project_path),
            );
        }
        "off" => {}
        _ => unreachable!(),
    }
    prune_empty_hooks(&mut settings);
    fs::write(
        hooks_file,
        format!("{}\n", serde_json::to_string_pretty(&settings)?),
    )?;
    Ok(())
}

fn delivery_mode(store: &Store, agent_type: &str, project_path: &Path) -> Result<&'static str> {
    if matches!(agent_type, "gemini" | "antigravity") {
        return Ok(if hooks_file(agent_type, project_path)?.exists() {
            "turn"
        } else {
            "off"
        });
    }
    let file = hooks_file(agent_type, project_path)?;
    if !file.exists() {
        return Ok("off");
    }
    let settings = read_json_file(&file)?;
    let has_start = event_has_owned_hook(&settings, "SessionStart", store);
    let has_stop = event_has_owned_hook(&settings, "Stop", store);
    Ok(match (has_start, has_stop) {
        (true, true) => "both",
        (true, false) => "monitor",
        (false, true) => "turn",
        (false, false) => "off",
    })
}

fn hooks_file(agent_type: &str, project_path: &Path) -> Result<PathBuf> {
    match agent_type {
        "claude-code" => Ok(project_path.join(".claude").join("settings.local.json")),
        "codex" => Ok(project_path.join(".codex").join("hooks.json")),
        "gemini" | "antigravity" => Ok(project_path.join(".agent").join("rules").join("ral.md")),
        _ => bail!("Unknown agent type: {agent_type}"),
    }
}

fn hook_command(store: &Store, command: &str, agent_type: &str, project_path: &Path) -> String {
    let script = store.scripts_dir().join(format!("{command}.sh"));
    format!(
        "'{}' '{}' '{}'",
        script.display(),
        agent_type,
        project_path.display()
    )
}

fn read_json_file(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let data = fs::read_to_string(path)?;
    if data.trim().is_empty() {
        return Ok(json!({}));
    }
    Ok(serde_json::from_str(&data)?)
}

fn strip_owned_event(settings: &mut Value, event: &str, store: &Store) {
    let Some(entries) = settings
        .pointer_mut(&format!("/hooks/{event}"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    entries.retain(|entry| !hook_entry_owned(entry, store));
}

fn add_hook_event(settings: &mut Value, event: &str, command: String) {
    if !settings.is_object() {
        *settings = json!({});
    }
    let obj = settings.as_object_mut().expect("object created");
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let hooks_obj = hooks.as_object_mut().expect("object created");
    let entries = hooks_obj.entry(event).or_insert_with(|| json!([]));
    if !entries.is_array() {
        *entries = json!([]);
    }
    entries.as_array_mut().expect("array created").push(json!({
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": command,
        }]
    }));
}

fn prune_empty_hooks(settings: &mut Value) {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    hooks.retain(|_, value| value.as_array().is_none_or(|array| !array.is_empty()));
    if hooks.is_empty() {
        settings.as_object_mut().map(|obj| obj.remove("hooks"));
    }
}

fn hook_event_len(settings: &Value, event: &str) -> usize {
    settings
        .pointer(&format!("/hooks/{event}"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

fn event_has_owned_hook(settings: &Value, event: &str, store: &Store) -> bool {
    settings
        .pointer(&format!("/hooks/{event}"))
        .and_then(Value::as_array)
        .is_some_and(|entries| entries.iter().any(|entry| hook_entry_owned(entry, store)))
}

fn hook_entry_owned(entry: &Value, store: &Store) -> bool {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks.iter().any(|hook| {
                hook.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|cmd| {
                        cmd.contains(&store.root.to_string_lossy().to_string())
                            || cmd.contains("/scripts/session-start.sh")
                            || cmd.contains("/scripts/session-end.sh")
                            || cmd.contains("/scripts/check-inbox.sh")
                    })
            })
        })
}

fn emit_monitor_directive(store: &Store, agent_type: &str, project_path: &Path) -> Result<()> {
    let session_id = env::var("CLAUDE_CODE_SESSION_ID")
        .unwrap_or_else(|_| format!("ral-{}-{}", Utc::now().timestamp(), process::id()));
    if watcher_alive_for_session(store, &session_id)? {
        let pid = fs::read_to_string(watcher_pidfile(store, &session_id)).unwrap_or_default();
        println!();
        println!(
            "A ral watch stream is already running in this session (pid {}).",
            pid.trim()
        );
        return Ok(());
    }
    emit_monitor_directive_with_session(store, agent_type, project_path, &session_id);
    Ok(())
}

fn emit_monitor_directive_with_session(
    store: &Store,
    agent_type: &str,
    project_path: &Path,
    session_id: &str,
) {
    let watch = store.scripts_dir().join("watch.sh");
    println!();
    println!("AGMSG-DIRECTIVE: For this running session, invoke the Monitor tool now with:");
    println!(
        "  command: {} {} {} {}",
        watch.display(),
        session_id,
        project_path.display(),
        agent_type
    );
    println!("  description: agmsg inbox stream");
    println!("  persistent: true");
    println!();
    println!("Pass the command above verbatim; the session_id is already filled in.");
}

fn emit_stop_directive() {
    println!();
    println!("AGMSG-DIRECTIVE: If a ral/agmsg watch Monitor task is running in this session,");
    println!("find it with TaskList and stop it with TaskStop.");
}

fn watcher_pidfile(store: &Store, session_id: &str) -> PathBuf {
    store.run_dir().join(format!("watch.{session_id}.pid"))
}

fn watcher_alive_for_session(store: &Store, session_id: &str) -> Result<bool> {
    let pidfile = watcher_pidfile(store, session_id);
    if !pidfile.exists() {
        return Ok(false);
    }
    let pid = fs::read_to_string(pidfile)?.trim().parse::<i32>().ok();
    Ok(pid.is_some_and(process_alive))
}

fn watcher_counts(store: &Store) -> Result<(usize, usize)> {
    if !store.run_dir().exists() {
        return Ok((0, 0));
    }
    let mut alive = 0;
    let mut stale = 0;
    for entry in fs::read_dir(store.run_dir())? {
        let path = entry?.path();
        if !is_watch_pidfile(&path) {
            continue;
        }
        let pid = fs::read_to_string(&path)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok());
        if pid.is_some_and(process_alive) {
            alive += 1;
        } else {
            stale += 1;
        }
    }
    Ok((alive, stale))
}

fn kill_all_watchers(store: &Store) -> Result<usize> {
    if !store.run_dir().exists() {
        return Ok(0);
    }
    let mut killed = 0;
    for entry in fs::read_dir(store.run_dir())? {
        let path = entry?.path();
        if !is_watch_pidfile(&path) {
            continue;
        }
        let pid = fs::read_to_string(&path)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok());
        if let Some(pid) = pid
            && process_alive(pid)
            && process_looks_like_watcher(pid)
        {
            kill_pid(pid);
            killed += 1;
        }
        let _ = fs::remove_file(path);
    }
    Ok(killed)
}

fn kill_watcher_for_session(store: &Store, session_id: &str) -> Result<()> {
    let pidfile = watcher_pidfile(store, session_id);
    if pidfile.exists() {
        let pid = fs::read_to_string(&pidfile)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok());
        if let Some(pid) = pid
            && process_alive(pid)
            && process_looks_like_watcher(pid)
        {
            kill_pid(pid);
        }
        let _ = fs::remove_file(pidfile);
    }
    Ok(())
}

fn cleanup_stale_watchers(store: &Store) -> Result<()> {
    if !store.run_dir().exists() {
        return Ok(());
    }
    for entry in fs::read_dir(store.run_dir())? {
        let path = entry?.path();
        if !is_watch_pidfile(&path) {
            continue;
        }
        let alive = fs::read_to_string(&path)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
            .is_some_and(process_alive);
        if !alive {
            let _ = fs::remove_file(path);
        }
    }
    Ok(())
}

fn is_watch_pidfile(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("watch.") && name.ends_with(".pid"))
}

fn process_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

fn kill_pid(pid: i32) {
    unsafe {
        libc::kill(pid, SIGTERM);
    }
}

fn process_looks_like_watcher(pid: i32) -> bool {
    #[cfg(target_os = "linux")]
    {
        if let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) {
            let cmd = String::from_utf8_lossy(&cmdline).replace('\0', " ");
            return cmd.contains(" watch ")
                || cmd.contains("watch.sh")
                || cmd.contains(" ral watch");
        }
    }
    true
}

fn max_message_id(store: &Store, pairs: &BTreeSet<(String, String)>) -> Result<i64> {
    let Some(conn) = store.open_db_if_exists()? else {
        return Ok(0);
    };
    let mut stmt = conn.prepare("SELECT id, team, to_agent FROM messages ORDER BY id")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut max_id = 0;
    for row in rows {
        let (id, team, to) = row?;
        if pairs.contains(&(team, to)) {
            max_id = max_id.max(id);
        }
    }
    Ok(max_id)
}

fn json_string_field(input: &str, key: &str) -> Option<String> {
    serde_json::from_str::<Value>(input)
        .ok()
        .and_then(|value| value.get(key).and_then(Value::as_str).map(str::to_owned))
}

fn install(cmd_name: &str, update: bool) -> Result<()> {
    let skill_dir = agents_dir()?.join("skills").join(cmd_name);
    if update && !skill_dir.exists() {
        bail!("Not installed: {}", skill_dir.display());
    }
    fs::create_dir_all(skill_dir.join("scripts"))?;
    fs::create_dir_all(skill_dir.join("templates"))?;
    fs::create_dir_all(skill_dir.join("db"))?;
    fs::create_dir_all(skill_dir.join("teams"))?;
    fs::create_dir_all(skill_dir.join("agents"))?;
    fs::create_dir_all(skill_dir.join("run"))?;

    let store = Store {
        root: skill_dir.clone(),
    };
    store.init_db()?;
    ensure_default_config(&store)?;

    fs::write(skill_dir.join(".rally-rs"), b"")?;
    fs::write(skill_dir.join("SKILL.md"), codex_skill(cmd_name))?;
    fs::write(
        skill_dir.join("templates").join("cmd.codex.md"),
        codex_skill(cmd_name),
    )?;
    fs::write(
        skill_dir.join("templates").join("cmd.claude-code.md"),
        claude_command_template(cmd_name),
    )?;
    fs::write(skill_dir.join("agents").join("openai.yaml"), OPENAI_YAML)?;
    for (script, command) in WRAPPER_COMMANDS {
        let path = skill_dir.join("scripts").join(script);
        fs::write(&path, wrapper_script(command))?;
        make_executable(&path)?;
    }

    let claude_dir = home_dir()?.join(".claude").join("commands");
    if home_dir()?.join(".claude").exists() {
        fs::create_dir_all(&claude_dir)?;
        fs::write(
            claude_dir.join(format!("{cmd_name}.md")),
            claude_command_template(cmd_name),
        )?;
    }
    update_codex_writable_roots(&skill_dir, true)?;

    println!("Installed to {}", skill_dir.display());
    println!("Claude Code: /{cmd_name}");
    println!("Codex: ${cmd_name}");
    Ok(())
}

fn uninstall(yes: bool, keep_data: bool) -> Result<()> {
    let skills_dir = agents_dir()?.join("skills");
    if !skills_dir.exists() {
        println!("Nothing to remove (not installed?)");
        return Ok(());
    }
    let mut installs = Vec::new();
    for entry in fs::read_dir(&skills_dir)? {
        let path = entry?.path();
        if path.join(".rally-rs").exists() {
            installs.push(path);
        }
    }
    if installs.is_empty() {
        println!("Nothing to remove (not installed?)");
        return Ok(());
    }
    if !yes {
        println!("Refusing interactive uninstall in this CLI mode. Re-run with --yes.");
        return Ok(());
    }
    for skill_dir in installs {
        let name = skill_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("ral")
            .to_owned();
        let store = Store {
            root: skill_dir.clone(),
        };
        remove_owned_hooks_from_registered_projects(&store)?;
        let claude_cmd = home_dir()?
            .join(".claude")
            .join("commands")
            .join(format!("{name}.md"));
        let _ = fs::remove_file(claude_cmd);
        update_codex_writable_roots(&skill_dir, false)?;
        if keep_data {
            let _ = fs::remove_dir_all(skill_dir.join("scripts"));
            let _ = fs::remove_dir_all(skill_dir.join("templates"));
            let _ = fs::remove_dir_all(skill_dir.join("agents"));
            let _ = fs::remove_dir_all(skill_dir.join("run"));
            let _ = fs::remove_file(skill_dir.join("SKILL.md"));
            println!(
                "Removed skill assets from {} (kept data)",
                skill_dir.display()
            );
        } else {
            fs::remove_dir_all(&skill_dir)?;
            println!("Removed {}", skill_dir.display());
        }
    }
    Ok(())
}

fn remove_owned_hooks_from_registered_projects(store: &Store) -> Result<()> {
    for (_, config) in team_configs(store)? {
        for (_, agent) in config.agents {
            for reg in agent.registrations {
                if matches!(reg.agent_type.as_str(), "gemini" | "antigravity") {
                    let _ = fs::remove_file(hooks_file(&reg.agent_type, Path::new(&reg.project))?);
                    continue;
                }
                let file = hooks_file(&reg.agent_type, Path::new(&reg.project))?;
                if !file.exists() {
                    continue;
                }
                let mut settings = read_json_file(&file)?;
                for event in ["SessionStart", "SessionEnd", "Stop"] {
                    strip_owned_event(&mut settings, event, store);
                }
                prune_empty_hooks(&mut settings);
                fs::write(
                    &file,
                    format!("{}\n", serde_json::to_string_pretty(&settings)?),
                )?;
            }
        }
    }
    Ok(())
}

fn update_codex_writable_roots(skill_dir: &Path, add: bool) -> Result<()> {
    let config = home_dir()?.join(".codex").join("config.toml");
    if !config.exists() {
        return Ok(());
    }
    let mut text = fs::read_to_string(&config)?;
    let db = skill_dir.join("db").to_string_lossy().to_string();
    let teams = skill_dir.join("teams").to_string_lossy().to_string();
    fs::copy(&config, config.with_extension("toml.bak"))?;
    if add {
        for path in [db, teams] {
            if !text.contains(&path) {
                if let Some(pos) = text.find("writable_roots = [") {
                    if let Some(end) = text[pos..].find(']') {
                        let insert_at = pos + end;
                        let prefix = if text[pos..insert_at].contains('"') {
                            ", "
                        } else {
                            ""
                        };
                        text.insert_str(insert_at, &format!("{prefix}\"{path}\""));
                    }
                } else if text.contains("[sandbox_workspace_write]") {
                    text.push_str(&format!("\nwritable_roots = [\"{path}\"]\n"));
                } else {
                    text.push_str(&format!(
                        "\n[sandbox_workspace_write]\nwritable_roots = [\"{path}\"]\n"
                    ));
                }
            }
        }
    } else {
        for path in [db, teams] {
            text = text.replace(&format!("\"{path}\", "), "");
            text = text.replace(&format!(", \"{path}\""), "");
            text = text.replace(&format!("\"{path}\""), "");
        }
    }
    fs::write(config, text)?;
    Ok(())
}

fn agents_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join(".agents"))
}

fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| anyhow!("could not determine home directory"))
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn wrapper_script(command: &str) -> String {
    format!(
        r#"#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SKILL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
exec "${{RAL_BIN:-ral}}" --home "$SKILL_DIR" {command} "$@"
"#
    )
}

fn ral_command() -> String {
    env::var("RAL_BIN").unwrap_or_else(|_| "ral".to_owned())
}

fn print_skill(cmd: &str) {
    print!("{}", codex_skill(cmd));
}

fn codex_skill(cmd: &str) -> String {
    format!(
        r#"---
name: {cmd}
description: Cross-agent messaging via SQLite. Send messages between Claude Code, Codex, Gemini CLI, and other agents.
---

# {cmd}

Use this skill to coordinate work between CLI agents through the `ral` message
box. Always use the installed wrapper scripts in
`~/.agents/skills/{cmd}/scripts/`; do not edit the SQLite database, team JSON,
hook files, or Codex/Claude settings by hand.

## Identity

Before any action, resolve the active identity for the current project.

```bash
~/.agents/skills/{cmd}/scripts/whoami.sh "$(pwd)" codex
```

If no identity is registered, ask the user for a team name and agent name, then
join and enable the normal Codex delivery mode:

```bash
~/.agents/skills/{cmd}/scripts/join.sh <team> <agent_name> codex "$(pwd)"
~/.agents/skills/{cmd}/scripts/delivery.sh set turn codex "$(pwd)"
```

For Claude Code, use `claude-code` as the agent type and prefer monitor
delivery:

```bash
~/.agents/skills/{cmd}/scripts/join.sh <team> <agent_name> claude-code "$(pwd)"
~/.agents/skills/{cmd}/scripts/delivery.sh set monitor claude-code "$(pwd)"
```

If `whoami.sh` reports multiple matching identities, ask which agent name to
act as before sending. Use `actas <name>` only when the user explicitly wants to
switch the active sending role.

## Commands

- No arguments: check inbox with `inbox.sh <team> <agent>` and summarize new messages.
- `send <agent> <message>`: send with `send.sh <team> <from> <to> "<message>"`.
- `team`: list members with `team.sh <team>`.
- `history`: show recent team history with `history.sh <team> [agent]`.
- `mode monitor|turn|both|off`: update delivery with `delivery.sh set <mode> <agent_type> "$(pwd)"`.
- `actas <name>`: join or switch this session's sending role for the current project.
- `drop <name>`: remove that role with `reset.sh "$(pwd)" <agent_type> <name>`.

## Delivery Modes

- `monitor`: best for Claude Code; messages can arrive while the conversation is running.
- `turn`: best for Codex; check inbox between turns with hooks.
- `both`: use monitor plus turn as a fallback when the agent supports both.
- `off`: disable automatic checks and rely on manual inbox checks.

Follow any `AGMSG-DIRECTIVE` printed by delivery scripts. It tells the host
agent how to install or remove hooks for the selected mode.

## Operating Rules

- Messages are plain text; include enough context for the receiving agent to act.
- Check inbox before sending if the current turn depends on the other agent's latest reply.
- Use `team` when the target agent name is unclear.
- Use `history` when reconnecting to an ongoing discussion.
- Do not assume messages execute commands. They only deliver instructions or context.
"#
    )
}

fn claude_command_template(cmd: &str) -> String {
    format!(
        r#"---
description: Agent messaging - check inbox, send messages, view history
---

Use the installed ral wrappers in `~/.agents/skills/{cmd}/scripts/`.

First resolve identity:

```bash
~/.agents/skills/{cmd}/scripts/whoami.sh "$(pwd)" claude-code
```

If not joined, ask for team and agent name, then run:

```bash
~/.agents/skills/{cmd}/scripts/join.sh <team> <agent_name> claude-code "$(pwd)"
~/.agents/skills/{cmd}/scripts/delivery.sh set monitor claude-code "$(pwd)"
```

For monitor/both mode, follow any `AGMSG-DIRECTIVE` printed by `delivery.sh`.

Default action: run inbox immediately. Reply with `send.sh` when appropriate.
"#
    )
}

const WRAPPER_COMMANDS: &[(&str, &str)] = &[
    ("init-db.sh", "init-db"),
    ("send.sh", "send"),
    ("inbox.sh", "inbox"),
    ("history.sh", "history"),
    ("join.sh", "join"),
    ("leave.sh", "leave"),
    ("rename-team.sh", "rename-team"),
    ("rename.sh", "rename"),
    ("team.sh", "team"),
    ("whoami.sh", "whoami"),
    ("identities.sh", "identities"),
    ("reset.sh", "reset"),
    ("config.sh", "config"),
    ("delivery.sh", "delivery"),
    ("hook.sh", "hook"),
    ("check-inbox.sh", "check-inbox"),
    ("watch.sh", "watch"),
    ("session-start.sh", "session-start"),
    ("session-end.sh", "session-end"),
];

const OPENAI_YAML: &str = "name: ral\n";

fn team_configs(store: &Store) -> Result<Vec<(String, TeamConfig)>> {
    if !store.teams_dir().exists() {
        return Ok(Vec::new());
    }
    let mut configs = Vec::new();
    for entry in fs::read_dir(store.teams_dir())? {
        let entry = entry?;
        let path = entry.path().join("config.json");
        if path.exists() {
            let name = entry.file_name().to_string_lossy().to_string();
            configs.push((name, read_team_config(&path)?));
        }
    }
    configs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(configs)
}

fn read_team_config(path: &Path) -> Result<TeamConfig> {
    let data = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&data)?;

    let config: TeamConfig = serde_json::from_value(value.clone()).or_else(|_| {
        let name = value
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned();
        let created_at = value
            .get("created_at")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned();
        let mut agents = BTreeMap::new();
        if let Some(obj) = value.get("agents").and_then(|v| v.as_object()) {
            for (agent, raw) in obj {
                if let Some(registrations) = raw.get("registrations").and_then(|v| v.as_array()) {
                    let registrations = registrations
                        .iter()
                        .cloned()
                        .map(serde_json::from_value)
                        .collect::<std::result::Result<Vec<Registration>, _>>()?;
                    agents.insert(agent.clone(), AgentConfig { registrations });
                } else if let (Some(agent_type), Some(project)) = (
                    raw.get("type").and_then(|v| v.as_str()),
                    raw.get("project").and_then(|v| v.as_str()),
                ) {
                    agents.insert(
                        agent.clone(),
                        AgentConfig {
                            registrations: vec![Registration {
                                agent_type: agent_type.to_owned(),
                                project: project.to_owned(),
                            }],
                        },
                    );
                }
            }
        }
        Ok::<_, serde_json::Error>(TeamConfig {
            name,
            agents,
            created_at,
        })
    })?;
    Ok(config)
}

fn write_team_config(path: &Path, config: &TeamConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(config)?;
    fs::write(path, format!("{data}\n"))?;
    Ok(())
}

fn validate_agent_type(agent_type: &str) -> Result<()> {
    match agent_type {
        "claude-code" | "codex" | "gemini" | "antigravity" => Ok(()),
        _ => bail!(
            "Unknown agent type: '{agent_type}' (supported: claude-code, codex, gemini, antigravity)"
        ),
    }
}

fn normalize_project_path(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn comma_or_none<I, S>(items: I) -> String
where
    I: Iterator<Item = S>,
    S: AsRef<str>,
{
    let values = items
        .map(|item| item.as_ref().to_owned())
        .collect::<Vec<_>>();
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(",")
    }
}

fn ensure_default_config(store: &Store) -> Result<PathBuf> {
    let path = store.config_path();
    if !path.exists() {
        fs::create_dir_all(store.db_dir())?;
        fs::write(&path, DEFAULT_CONFIG)?;
    }
    Ok(path)
}

fn config_get(store: &Store, key: &str, default: Option<&str>) -> Result<String> {
    if !store.config_path().exists() {
        return Ok(default.unwrap_or("").to_owned());
    }
    let data = fs::read_to_string(store.config_path())?;
    Ok(yaml_get(&data, key)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default.unwrap_or("").to_owned()))
}

fn config_set(store: &Store, key: &str, value: &str) -> Result<()> {
    let path = ensure_default_config(store)?;
    let mut data = fs::read_to_string(&path)?;
    yaml_set(&mut data, key, value);
    fs::write(path, data)?;
    Ok(())
}

fn yaml_get<'a>(data: &'a str, key: &str) -> Option<&'a str> {
    let (section, field) = split_key(key);
    let mut in_section = section.is_none();
    for line in data.lines() {
        if !line.starts_with(' ') && !line.starts_with('#') && !line.trim().is_empty() {
            in_section = section.is_some_and(|s| line.trim_end() == format!("{s}:"));
        }
        if in_section {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix(&format!("{field}:")) {
                return Some(rest.split('#').next().unwrap_or("").trim());
            }
        }
    }
    None
}

fn yaml_set(data: &mut String, key: &str, value: &str) {
    let (section, field) = split_key(key);
    let lines = data.lines().map(str::to_owned).collect::<Vec<_>>();

    if let Some(section) = section {
        let mut out = Vec::new();
        let mut in_section = false;
        let mut section_seen = false;
        let mut field_set = false;

        for line in lines {
            if !line.starts_with(' ') && !line.starts_with('#') && !line.trim().is_empty() {
                if in_section && !field_set {
                    out.push(format!("  {field}: {value}"));
                    field_set = true;
                }
                in_section = line.trim_end() == format!("{section}:");
                section_seen |= in_section;
            }

            if in_section && line.trim_start().starts_with(&format!("{field}:")) {
                out.push(format!("  {field}: {value}"));
                field_set = true;
            } else {
                out.push(line);
            }
        }

        if section_seen {
            if in_section && !field_set {
                out.push(format!("  {field}: {value}"));
            }
        } else {
            if !out.last().is_some_and(|line| line.is_empty()) {
                out.push(String::new());
            }
            out.push(format!("{section}:"));
            out.push(format!("  {field}: {value}"));
        }
        *data = format!("{}\n", out.join("\n"));
    } else {
        let mut out = Vec::new();
        let mut field_set = false;
        for line in lines {
            if !line.starts_with(' ') && line.trim_start().starts_with(&format!("{field}:")) {
                out.push(format!("{field}: {value}"));
                field_set = true;
            } else {
                out.push(line);
            }
        }
        if !field_set {
            out.push(format!("{field}: {value}"));
        }
        *data = format!("{}\n", out.join("\n"));
    }
}

fn split_key(key: &str) -> (Option<&str>, &str) {
    key.split_once('.')
        .map(|(section, field)| (Some(section), field))
        .unwrap_or((None, key))
}

const DEFAULT_CONFIG: &str = r#"# rally-rs configuration
delivery:
  monitor:
    poll_interval: 5
  turn:
    check_interval: 60
"#;
