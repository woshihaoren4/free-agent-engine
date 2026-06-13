use crate::Env;
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::{Arc, RwLock};

#[derive(Debug)]
pub struct Context {
    pub env: Env,
    pub extend: Arc<RwLock<HashMap<String, String>>>,
}
impl Clone for Context {
    fn clone(&self) -> Self {
        Self {
            env: self.env.clone(),
            extend: self.extend.clone(),
        }
    }
}
impl Context {
    pub fn new(env: Env) -> Self {
        Self {
            env,
            extend: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    pub fn set<K, V>(&self, key: K, value: V)
    where
        K: Into<String>,
        V: Into<String>,
    {
        let mut extend = self.extend.write().unwrap();
        extend.insert(key.into(), value.into());
    }
    pub fn get(&self, key: &str) -> Option<String> {
        let extend = self.extend.read().unwrap();
        extend.get(key).map(|v| v.clone())
    }
}
