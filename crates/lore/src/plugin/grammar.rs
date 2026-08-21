//! Runtime `.wasm` tree-sitter grammars, and the per-thread parser pool they
//! need.
//!
//! # Threading
//!
//! The spike settled the rules and they are not negotiable:
//!
//! * one [`Engine`] for the process — cheap to hold, `Send + Sync`, and the
//!   thing that owns compiled module caching;
//! * one `WasmStore` **per thread**, never shared, and `set_wasm_store` *moves*
//!   the store into the parser, so the parser is the store's owner from then on;
//! * a `Language` clones freely across stores and threads, so a grammar is
//!   loaded **once**, at registry load, and every thread's parser is handed a
//!   clone of the same `Language`.
//!
//! Store creation costs ~7–10 ms and a grammar load 16–60 ms, so per-file
//! construction is out of the question: at a few hundred plugin-routed files a
//! pass that is tens of seconds of pure setup. [`Grammar::parse`] therefore
//! keeps a small thread-local cache of ready parsers keyed by grammar.
//!
//! # Why thread-*local* and not a global pool
//!
//! `chunk_file` is called from `daemon::index::index_one`, in a plain `for`
//! loop over the snapshot manifest, inside a single `spawn_blocking` task —
//! one pass at a time, sequential within a pass. So in practice one thread
//! chunks a whole pass and pays the setup once; the pool exists because tokio's
//! blocking pool hands successive passes to *different* threads, and because
//! nothing in the chunker's contract promises the sequential shape will last.
//! A `Mutex`-guarded global pool would serialize any future parallel pass on
//! the one thing that must never be shared anyway.

use crate::plugin::Unavailable;

#[cfg(feature = "wasm-grammars")]
pub use enabled::Grammar;

#[cfg(not(feature = "wasm-grammars"))]
pub use disabled::Grammar;

/// Loads a `.wasm` grammar artifact, or says precisely why it cannot.
///
/// `symbol` is the suffix of the module's exported `tree_sitter_<symbol>`
/// function; `bytes` is the artifact.
pub(crate) fn load(symbol: &str, bytes: &[u8]) -> Result<Grammar, Unavailable> {
    #[cfg(feature = "wasm-grammars")]
    {
        enabled::load(symbol, bytes)
    }
    #[cfg(not(feature = "wasm-grammars"))]
    {
        let _ = (symbol, bytes);
        Err(Unavailable::WasmUnsupported)
    }
}

#[cfg(not(feature = "wasm-grammars"))]
mod disabled {
    /// A loaded grammar — uninhabited in a build without `wasm-grammars`, so
    /// the routing code that handles it is still type-checked while the engine
    /// itself is absent. Nothing can construct one, and [`super::load`] says so
    /// with [`super::Unavailable::WasmUnsupported`].
    #[derive(Debug)]
    pub enum Grammar {}

    impl Grammar {
        pub(crate) fn parse(&self, _src: &str) -> Option<tree_sitter::Tree> {
            match *self {}
        }
    }
}

#[cfg(feature = "wasm-grammars")]
mod enabled {
    use std::cell::RefCell;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};

    use tree_sitter::{
        LANGUAGE_VERSION, Language, MIN_COMPATIBLE_LANGUAGE_VERSION, Parser, Tree, WasmStore,
        wasmtime::Engine,
    };

    use crate::plugin::Unavailable;

    /// How many ready parsers one thread keeps. A plugin set with more
    /// grammars than this still works; it just reconstructs the least
    /// recently claimed one. Eight is far past any plausible plugin set.
    const CACHED_PARSERS: usize = 8;

    thread_local! {
        /// Ready parsers for this thread, most-recently-used last. Each owns
        /// its own `WasmStore` (moved in by `set_wasm_store`), which is exactly
        /// the resource that must not be shared.
        static PARSERS: RefCell<Vec<(u64, Parser)>> = const { RefCell::new(Vec::new()) };
    }

    fn engine() -> &'static Engine {
        static ENGINE: OnceLock<Engine> = OnceLock::new();
        ENGINE.get_or_init(Engine::default)
    }

    /// Process-unique identity for a loaded grammar, so a thread's parser
    /// cache cannot serve a parser built for a *previous* registry's grammar
    /// of the same name.
    fn next_id() -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }

    /// A `.wasm` grammar loaded once and shared by every thread.
    #[derive(Debug)]
    pub struct Grammar {
        id: u64,
        language: Language,
    }

    impl Grammar {
        /// Parses `src` on this thread's parser for this grammar, creating one
        /// (store included) the first time.
        ///
        /// `None` on a store/parser failure or a grammar that refuses the
        /// source — the same signal the native path gives, so the caller
        /// degrades to line windows rather than losing the file.
        pub(crate) fn parse(&self, src: &str) -> Option<Tree> {
            PARSERS.with(|parsers| {
                let mut parsers = parsers.borrow_mut();
                if let Some(at) = parsers.iter().position(|(id, _)| *id == self.id) {
                    // Move to the back: the cache evicts from the front.
                    let entry = parsers.remove(at);
                    parsers.push(entry);
                    let last = parsers.len() - 1;
                    return parsers[last].1.parse(src, None);
                }
                let mut parser = Parser::new();
                let store = match WasmStore::new(engine()) {
                    Ok(store) => store,
                    Err(err) => {
                        tracing::warn!(error = %err, "creating a wasm store failed; \
                             this file falls back to line windows");
                        return None;
                    }
                };
                if let Err(err) = parser.set_wasm_store(store) {
                    tracing::warn!(error = %err, "attaching a wasm store to a parser failed");
                    return None;
                }
                if let Err(err) = parser.set_language(&self.language) {
                    tracing::warn!(error = %err, "setting a wasm grammar on a parser failed");
                    return None;
                }
                if parsers.len() >= CACHED_PARSERS {
                    parsers.remove(0);
                }
                parsers.push((self.id, parser));
                let last = parsers.len() - 1;
                parsers[last].1.parse(src, None)
            })
        }
    }

    pub(crate) fn load(symbol: &str, bytes: &[u8]) -> Result<Grammar, Unavailable> {
        // A throwaway store purely to compile the module. The `Language` it
        // returns outlives it and is what every thread's own store is handed.
        //
        // This is the first call into tree-sitter's C runtime, so on Windows it
        // is also where a mislinked build dies — not with an error this can map
        // to `Unavailable`, but with a hard STATUS_ACCESS_VIOLATION, because the
        // wasmtime C API entry points behind it were garbage collected at link
        // time. Nothing here can defend against that; the fix is the
        // `/OPT:NOREF` flag in this repo's `.cargo/config.toml`, which explains
        // the mechanism. If the daemon dies between "listening" and "chunker
        // plugins loaded" with no panic and no log line, that is this.
        let mut store = WasmStore::new(engine()).map_err(|err| Unavailable::EngineFailed {
            detail: err.to_string(),
        })?;
        let language = store
            .load_language(symbol, bytes)
            .map_err(|err| Unavailable::Rejected {
                kind: format!("{:?}", err.kind),
                detail: err.message,
            })?;
        let abi = language.abi_version();
        if !(MIN_COMPATIBLE_LANGUAGE_VERSION..=LANGUAGE_VERSION).contains(&abi) {
            return Err(Unavailable::AbiUnsupported {
                abi,
                min: MIN_COMPATIBLE_LANGUAGE_VERSION,
                max: LANGUAGE_VERSION,
            });
        }
        if !language.is_wasm() {
            return Err(Unavailable::NotWasm);
        }
        Ok(Grammar {
            id: next_id(),
            language,
        })
    }
}
