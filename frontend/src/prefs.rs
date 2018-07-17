use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::env;

use failure::Error;
use serde_json;

use Command;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Prefs {
    aliases: HashMap<String, Command>,
}

impl Prefs {
    pub fn load() -> Result<Prefs, Error> {
        let mut contents = String::new();
        let mut f = File::open(env::home_dir().unwrap().join(".cache").join("nak").join("prefs.nak"))?;
        f.read_to_string(&mut contents)?;

        let prefs: Prefs = serde_json::from_str(&contents)?;
        Ok(prefs)
    }
}