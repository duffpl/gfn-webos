//! Start-up on a worker thread: spawn the core, sign in (device code) and fetch the library.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use gfn_core_client::{CoreClient, CoreEvent, SpawnOptions};
use serde_json::{Value, json};

use crate::{Update, Updates, art, session::sleep_unless_quit};

#[derive(Debug, Clone)]
pub struct Game {
    pub app_id: Value,
    pub title: String,
    pub art: Option<String>,
    /// NVIDIA's genre codes; [`Progress::Genres`] names them.
    pub genres: Vec<String>,
    /// Lower-case title, publisher, developer, stores and genres, for search.
    pub search_text: String,
    pub variants: Vec<Variant>,
}

#[derive(Debug, Clone)]
pub struct Variant {
    pub id: String,
    pub store: String,
    pub selected: bool,
    pub in_library: bool,
    pub hdr: bool,
}

impl Game {
    /// The core requires `appId == variantId`: launch the store the user picked in GFN.
    pub fn launch_variant(&self) -> Option<&Variant> {
        self.variants
            .iter()
            .find(|v| v.selected)
            .or_else(|| self.variants.iter().find(|v| v.in_library))
            .or(self.variants.first())
    }

    /// Whether the store version that would launch streams in HDR.
    pub fn hdr(&self) -> bool {
        self.launch_variant().is_some_and(|v| v.hdr)
    }

    pub fn has_id(&self, id: &str) -> bool {
        self.variants.iter().any(|v| v.id == id) || self.app_id.as_str() == Some(id) || self.app_id.to_string() == id
    }
}

pub enum Progress {
    Starting(String),
    SignIn { code: String, uri: String, uri_complete: String },
    SignedIn { core: Arc<CoreClient>, user: String, scope: Value },
    Library(Vec<Game>),
    /// Genre code to display name, once known.
    Genres(HashMap<String, String>),
    Failed(String),
}

pub struct Config {
    pub core: PathBuf,
    pub data_dir: PathBuf,
    pub verbose: bool,
}

pub fn start(config: Config, art: Receiver<art::Message>, art_tx: Sender<art::Message>, updates: Updates, quit: Arc<AtomicBool>) {
    let spawned = std::thread::Builder::new().name("gfn-account".into()).spawn({
        let updates = updates.clone();
        move || {
            let send = |progress| {
                let _ = updates.send(Update::Account(progress));
            };
            if let Err(error) = run(&config, art, art_tx, &updates, &quit, &send) {
                send(Progress::Failed(error));
            }
        }
    });
    if let Err(error) = spawned {
        let _ = updates.send(Update::Account(Progress::Failed(format!("start-up thread: {error}"))));
    }
}

fn run(
    config: &Config,
    art: Receiver<art::Message>,
    art_tx: Sender<art::Message>,
    updates: &Updates,
    quit: &AtomicBool,
    send: &dyn Fn(Progress),
) -> Result<(), String> {
    send(Progress::Starting("Starting".into()));
    std::fs::create_dir_all(&config.data_dir).map_err(|e| format!("{}: {e}", config.data_dir.display()))?;
    let (core, events) = CoreClient::spawn(SpawnOptions {
        executable: &config.core,
        data_dir: Some(&config.data_dir),
        shell_version: env!("CARGO_PKG_VERSION"),
        diagnostics: Some(core_log(&config.data_dir, config.verbose)?),
    })
    .map_err(|e| format!("Could not start the GeForce NOW core: {e}"))?;
    let core = Arc::new(core);
    forward_events(events, art_tx);
    art::spawn(Arc::clone(&core), art, updates.clone());

    let call = |method: &str, params: Value, secs: u64| {
        core.request(method, params, Duration::from_secs(secs)).map_err(|e| format!("{method}: {e}"))
    };
    let session = signed_in_session(&call, quit, send)?;
    let scope = json!({
        "generation": session["generation"],
        "userId": session["session"]["user"]["userId"],
        "providerIdpId": session["session"]["provider"]["idpId"],
    });
    let user = session["session"]["user"]["displayName"].as_str().unwrap_or("").to_owned();
    send(Progress::SignedIn { core: Arc::clone(&core), user, scope });
    send(Progress::Library(library(&call)?));
    // Codes stand in for names until (or unless) these arrive.
    match call("catalog.definitions.get", json!({}), 30) {
        Ok(definitions) => send(Progress::Genres(genre_names(&definitions))),
        Err(error) => eprintln!("genre names: {error}"),
    }
    Ok(())
}

fn forward_events(events: Receiver<CoreEvent>, art: Sender<art::Message>) {
    let forward = move || {
        for event in events {
            if event.name == "artwork.ready" {
                let url = event.payload["sourceUrl"].as_str().unwrap_or("").to_owned();
                let path = event.payload["filePath"].as_str().map(str::to_owned);
                if art.send(art::Message::Ready { url, path }).is_err() {
                    return;
                }
            }
        }
    };
    if let Err(error) = std::thread::Builder::new().name("gfn-core-events".into()).spawn(forward) {
        eprintln!("core event thread: {error}");
    }
}

/// Core diagnostics go to a file: they can carry account identifiers that should not end up in
/// terminal scrollback.
fn core_log(data_dir: &Path, echo: bool) -> Result<Box<dyn FnMut(&str) + Send>, String> {
    use std::io::Write;
    let path = data_dir.join("core.log");
    let mut file = std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("core diagnostics: {}", path.display());
    Ok(Box::new(move |line| {
        let _ = writeln!(file, "{line}");
        if echo {
            eprintln!("core: {line}");
        }
    }))
}

type Call<'a> = dyn Fn(&str, Value, u64) -> Result<Value, String> + 'a;

fn signed_in_session(call: &Call<'_>, quit: &AtomicBool, send: &dyn Fn(Progress)) -> Result<Value, String> {
    let current = call("auth.session.get", json!({}), 10)?;
    if !current["session"].is_null() {
        return Ok(current);
    }
    let attempt = call("auth.device.start", json!({}), 30)?;
    let attempt_id = attempt["attemptId"].clone();
    let text = |key: &str| attempt[key].as_str().unwrap_or("").to_owned();
    send(Progress::SignIn {
        code: text("userCode"),
        uri: text("verificationUri"),
        uri_complete: text("verificationUriComplete"),
    });
    let mut wait = Duration::from_secs(attempt["intervalSeconds"].as_u64().unwrap_or(5));
    loop {
        if sleep_unless_quit(wait, quit) {
            let _ = call("auth.device.cancel", json!({"attemptId": attempt_id}), 10);
            return Err("Sign-in was cancelled".into());
        }
        let poll = call("auth.device.poll", json!({"attemptId": attempt_id}), 30)?;
        match poll["status"].as_str() {
            Some("authorized") => break,
            Some("pending" | "slow_down") => {
                wait = Duration::from_millis(poll["retryAfterMs"].as_u64().unwrap_or(5000).max(1000));
            }
            other => {
                return Err(format!("Sign-in ended ({}): {}", other.unwrap_or("?"), poll["message"].as_str().unwrap_or("")));
            }
        }
    }
    send(Progress::Starting("Signing in".into()));
    call("auth.device.complete", json!({"attemptId": attempt_id, "staySignedIn": true}), 30)
}

const PAGE: u64 = 100;
const MAX_GAMES: usize = 1000;

fn library(call: &Call<'_>) -> Result<Vec<Game>, String> {
    // The shell names the traversal; the core binds every continuation page to it.
    let started = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let traversal = format!("{}:{}", started.as_millis(), std::process::id());
    let mut games = Vec::new();
    let mut params = json!({"limit": PAGE, "cursor": "", "traversalId": traversal, "catalogRevision": null, "catalogContext": ""});
    loop {
        let page = call("catalog.library.list", params.clone(), 60)?;
        games.extend(page["games"].as_array().into_iter().flatten().map(game));
        if page["hasNextPage"] != true || games.len() >= MAX_GAMES {
            return Ok(games);
        }
        params = json!({
            "limit": PAGE,
            "cursor": page["nextCursor"],
            "traversalId": traversal,
            "catalogRevision": page["catalogRevision"],
            "catalogContext": page["catalogContext"],
        });
    }
}

fn genre_names(definitions: &Value) -> HashMap<String, String> {
    definitions["genres"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| Some((item["genre"].as_str()?.to_owned(), item["label"].as_str()?.to_owned())))
        .collect()
}

fn game(value: &Value) -> Game {
    let text = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
    let title = text(&value["title"]).unwrap_or_else(|| "Untitled".into());
    Game {
        app_id: value["id"].clone(),
        art: text(&value["imageUrl"]).or_else(|| text(&value["keyArtUrl"])).or_else(|| text(&value["heroImageUrl"])),
        genres: value["genres"].as_array().into_iter().flatten().filter_map(text).collect(),
        search_text: text(&value["searchText"]).unwrap_or_else(|| title.to_lowercase()),
        title,
        variants: value["variants"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| {
                Some(Variant {
                    id: text(&v["id"])?,
                    store: text(&v["appStore"]).unwrap_or_default(),
                    selected: v["librarySelected"] == true,
                    in_library: v["inLibrary"] == true,
                    hdr: v["hdr"] == true,
                })
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_entries_keep_genres_search_text_and_hdr() {
        let game = game(&json!({
            "id": "cms-1", "title": "Cyberpunk 2077", "genres": ["ACTION", "RPG"], "searchText": "cyberpunk 2077 cd projekt red steam",
            "variants": [{"id": "1001", "appStore": "STEAM", "librarySelected": true, "inLibrary": true, "hdr": true}],
        }));
        assert_eq!(game.genres, ["ACTION", "RPG"]);
        assert!(game.search_text.contains("projekt"));
        assert!(game.hdr());
        let bare = super::game(&json!({"id": "cms-2", "title": "Portal 2", "variants": [{"id": "620"}]}));
        assert_eq!(bare.search_text, "portal 2");
        assert!(!bare.hdr());
    }

    #[test]
    fn genre_names_come_from_the_definitions_section() {
        let names = genre_names(&json!({"genres": {"metadataKind": "genres", "items": [{"genre": "RPG", "label": "Role-Playing"}, {"genre": "X"}]}}));
        assert_eq!(names, HashMap::from([("RPG".to_owned(), "Role-Playing".to_owned())]));
    }
}
