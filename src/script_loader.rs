// ABOUTME: Runtime-loadable JS scripts with compiled-in defaults and directory override
// ABOUTME: Load from DRAVR_SCIOTTE_SCRIPTS_DIR if set, otherwise use embedded scripts
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tokio::fs;
use tokio::sync::RwLock;
use tracing::{debug, info};

/// TTL for cached script reads from the filesystem
const CACHE_TTL_SECS: u64 = 300;

/// Global script loader instance
static LOADER: OnceLock<ScriptLoader> = OnceLock::new();

/// Get the global script loader
pub fn loader() -> &'static ScriptLoader {
    LOADER.get_or_init(ScriptLoader::new)
}

/// Runtime-loadable JS script loader.
/// Checks `DRAVR_SCIOTTE_SCRIPTS_DIR` for overrides, falls back to compiled-in defaults.
/// Caches filesystem reads with a TTL to avoid repeated I/O.
pub struct ScriptLoader {
    scripts_dir: Option<PathBuf>,
    cache: RwLock<HashMap<String, CachedScript>>,
}

struct CachedScript {
    content: String,
    loaded_at: Instant,
}

impl ScriptLoader {
    fn new() -> Self {
        let scripts_dir = env::var("DRAVR_SCIOTTE_SCRIPTS_DIR")
            .ok()
            .map(PathBuf::from);

        if let Some(ref dir) = scripts_dir {
            info!(dir = %dir.display(), "Script loader using override directory");
        }

        Self {
            scripts_dir,
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Load a script by name. Checks override directory first, then compiled-in defaults.
    pub async fn load(&self, name: &str) -> String {
        // Try override directory with cache
        if let Some(ref dir) = self.scripts_dir {
            // Check cache
            {
                let cache = self.cache.read().await;
                if let Some(cached) = cache.get(name) {
                    if cached.loaded_at.elapsed() < Duration::from_secs(CACHE_TTL_SECS) {
                        return cached.content.clone();
                    }
                }
            }

            // Read from filesystem
            let path = dir.join(name);
            if let Ok(content) = fs::read_to_string(&path).await {
                debug!(name, path = %path.display(), "Loaded script from override directory");
                let mut cache = self.cache.write().await;
                cache.insert(
                    name.to_owned(),
                    CachedScript {
                        content: content.clone(),
                        loaded_at: Instant::now(),
                    },
                );
                return content;
            }
        }

        // Fall back to compiled-in default
        default_script(name).to_owned()
    }
}

/// Compiled-in default scripts
fn default_script(name: &str) -> &'static str {
    match name {
        "dismiss_cookie.js" => include_str!("../scripts/js/dismiss_cookie.js"),
        "extract_number.js" => include_str!("../scripts/js/extract_number.js"),
        "parse_2fa_options.js" => include_str!("../scripts/js/parse_2fa_options.js"),
        "enter_password_coords.js" => include_str!("../scripts/js/enter_password_coords.js"),
        "element_exists.js" => include_str!("../scripts/js/element_exists.js"),
        "get_element_center.js" => include_str!("../scripts/js/get_element_center.js"),
        "click_element.js" => include_str!("../scripts/js/click_element.js"),
        _ => {
            tracing::warn!(name, "Unknown script requested, returning empty");
            ""
        }
    }
}
