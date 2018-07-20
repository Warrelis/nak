use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::env;
use std::io;

use failure::Error;
use serde_json;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Item {
    Literal(String),
    Variable(usize),
    Expando(usize),
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub struct Rule {
    find: Vec<Item>,
    replace: Vec<Item>,
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub struct Prefs {
    aliases: Vec<Rule>,
}

impl Prefs {
    pub fn load() -> Result<Prefs, Error> {
        let mut contents = String::new();
        match File::open(env::home_dir().unwrap().join(".config").join("nak").join("prefs.nak")) {
            Ok(mut f) => {
                f.read_to_string(&mut contents)?;

                let prefs: Prefs = serde_json::from_str(&contents)?;
                Ok(prefs)
            }
            Err(e) => {
                if e.kind() == io::ErrorKind::NotFound {
                    Ok(Prefs::default())
                } else {
                    Err(e.into())
                }
            }
        }
    }

    pub fn expand(&self, cmd: Vec<String>) -> Vec<String> {

        'outer: for rule in &self.aliases {
            let mut var_matches = HashMap::new();
            let mut expando_matches = HashMap::new();

            let mut it = cmd.iter();
            let mut next = it.next();

            for item in &rule.find {
                match item {
                    Item::Literal(word) => {
                        if next == Some(word) {
                            next = it.next();
                        } else {
                            continue 'outer;
                        }
                    }
                    Item::Variable(id) => {
                        assert!(!var_matches.contains_key(id));
                        assert!(!expando_matches.contains_key(id));

                        if let Some(word) = next {
                            var_matches.insert(id, word.clone());

                            next = it.next();
                        } else {
                            continue 'outer;
                        }
                    }
                    Item::Expando(id) => {
                        assert!(!var_matches.contains_key(id));
                        assert!(!expando_matches.contains_key(id));

                        let mut words = Vec::new();
                        if let Some(next) = next {
                            words.push(next.clone());
                            for i in &mut it {
                                words.push(i.clone());
                            }
                        }
                        expando_matches.insert(id, words);
                    }
                }
            }

            // Successfully matched, now construct the output!
            let mut res = Vec::new();
            for item in &rule.replace {
                match item {
                    Item::Literal(word) => res.push(word.clone()),
                    Item::Variable(id) => res.push(var_matches.remove(id).unwrap()),
                    Item::Expando(id) => res.extend(expando_matches.remove(id).unwrap()),
                }
            }

            return res;
        }

        cmd
    }
}
