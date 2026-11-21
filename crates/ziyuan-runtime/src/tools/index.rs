//! 轻量增量符号与标识符索引缓存（`.ziyuan/index/symbols-v1.json`）。
//!
//! - 零新增依赖（仅用已有 `serde_json`），仅作加速衍生缓存：损坏、版本不匹配或写盘失败时自动静默重建，绝不阻断工具调用；
//! - 按 `(mtime_secs, mtime_nanos, size)` 指纹即时失效；
//! - 仅在项目根已存在 `.ziyuan/` 目录时落盘持久化，进程内 `Mutex` 缓存始终生效；
//! - 暂存区（staged）改动永不走缓存，确保未落盘修改始终实时可见；
//! - 为 `find_references` 附带 256 位标识符布隆过滤器（严格与 `is_ref_ident_char` 词法一致，零假阴性），
//!   大仓库二次检索时可直接跳过未变更且不含目标标识符的文件，无需重新读盘或剥离注释。

use super::symbols::{self, Symbol};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fs::{self, Metadata};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

const INDEX_VERSION: u64 = 1;
const INDEX_REL_DIR: &str = ".ziyuan/index";
const INDEX_FILE_NAME: &str = "symbols-v1.json";
const MAX_INDEX_ENTRIES: usize = 20_000;

/// 单个文件的 `(mtime, size)` 变更指纹。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FileStamp {
    pub mtime_secs: u64,
    pub mtime_nanos: u32,
    pub size: u64,
}

impl FileStamp {
    pub(super) fn from_metadata(meta: &Metadata) -> Option<Self> {
        let modified = meta.modified().ok()?;
        let dur = modified.duration_since(UNIX_EPOCH).ok()?;
        Some(Self {
            mtime_secs: dur.as_secs(),
            mtime_nanos: dur.subsec_nanos(),
            size: meta.len(),
        })
    }

    fn to_json(self) -> Value {
        json!([self.mtime_secs, self.mtime_nanos, self.size])
    }

    fn from_json(v: &Value) -> Option<Self> {
        let arr = v.as_array()?;
        if arr.len() != 3 {
            return None;
        }
        Some(Self {
            mtime_secs: arr[0].as_u64()?,
            mtime_nanos: u32::try_from(arr[1].as_u64()?).ok()?,
            size: arr[2].as_u64()?,
        })
    }
}

#[derive(Clone, Debug)]
struct CachedSymbol {
    line: usize,
    end_line: usize,
    kind: &'static str,
    name: String,
    parent: Option<String>,
    signature: String,
    depth: usize,
}

impl CachedSymbol {
    fn from_symbol(sym: &Symbol) -> Self {
        Self {
            line: sym.line,
            end_line: sym.end_line,
            kind: sym.kind,
            name: sym.name.clone(),
            parent: sym.parent.clone(),
            signature: sym.signature.clone(),
            depth: sym.depth,
        }
    }

    fn to_symbol(&self) -> Symbol {
        Symbol {
            line: self.line,
            end_line: self.end_line,
            kind: self.kind,
            name: self.name.clone(),
            parent: self.parent.clone(),
            signature: self.signature.clone(),
            depth: self.depth,
        }
    }

    fn to_json(&self) -> Value {
        json!([
            self.line,
            self.end_line,
            self.kind,
            self.name,
            self.parent,
            self.signature,
            self.depth
        ])
    }

    fn from_json(v: &Value) -> Option<Self> {
        let arr = v.as_array()?;
        if arr.len() != 7 {
            return None;
        }
        let line = usize::try_from(arr[0].as_u64()?).ok()?;
        let end_line = usize::try_from(arr[1].as_u64()?).ok()?;
        let kind = intern_kind(arr[2].as_str()?)?;
        let name = arr[3].as_str()?.to_owned();
        let parent = match &arr[4] {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            _ => return None,
        };
        let signature = arr[5].as_str()?.to_owned();
        let depth = usize::try_from(arr[6].as_u64()?).ok()?;
        Some(Self {
            line,
            end_line,
            kind,
            name,
            parent,
            signature,
            depth,
        })
    }
}

fn intern_kind(raw: &str) -> Option<&'static str> {
    match raw {
        "fn" => Some("fn"),
        "struct" => Some("struct"),
        "enum" => Some("enum"),
        "trait" => Some("trait"),
        "impl" => Some("impl"),
        "mod" => Some("mod"),
        "const" => Some("const"),
        "static" => Some("static"),
        "type" => Some("type"),
        "macro" => Some("macro"),
        "class" => Some("class"),
        "interface" => Some("interface"),
        "var" => Some("var"),
        "namespace" => Some("namespace"),
        "typedef" => Some("typedef"),
        "heading" => Some("heading"),
        "method" => Some("method"),
        "union" => Some("union"),
        _ => None,
    }
}

#[derive(Clone, Debug)]
struct CachedFile {
    stamp: FileStamp,
    symbols: Vec<CachedSymbol>,
    ident_bloom: Option<[u64; 4]>,
}

impl CachedFile {
    fn to_json(&self) -> Value {
        let syms: Vec<Value> = self.symbols.iter().map(CachedSymbol::to_json).collect();
        let mut obj = Map::new();
        obj.insert("s".into(), self.stamp.to_json());
        obj.insert("y".into(), Value::Array(syms));
        if let Some(bloom) = self.ident_bloom {
            obj.insert("b".into(), json!(bloom));
        }
        Value::Object(obj)
    }

    fn from_json(v: &Value) -> Option<Self> {
        let obj = v.as_object()?;
        let stamp = FileStamp::from_json(obj.get("s")?)?;
        let symbols = obj
            .get("y")?
            .as_array()?
            .iter()
            .map(CachedSymbol::from_json)
            .collect::<Option<Vec<_>>>()?;
        let ident_bloom = match obj.get("b") {
            None | Some(Value::Null) => None,
            Some(Value::Array(arr)) if arr.len() == 4 => Some([
                arr[0].as_u64()?,
                arr[1].as_u64()?,
                arr[2].as_u64()?,
                arr[3].as_u64()?,
            ]),
            Some(_) => return None,
        };
        Some(Self {
            stamp,
            symbols,
            ident_bloom,
        })
    }
}

/// 进程内＋可选落盘（`.ziyuan/index/symbols-v1.json`）的增量符号索引缓存。
pub(super) struct SymbolIndex {
    root: PathBuf,
    loaded: bool,
    dirty: bool,
    entries: BTreeMap<String, CachedFile>,
}

impl SymbolIndex {
    pub(super) fn new(root: PathBuf) -> Self {
        Self {
            root,
            loaded: false,
            dirty: false,
            entries: BTreeMap::new(),
        }
    }

    fn cache_path(&self) -> PathBuf {
        self.root.join(INDEX_REL_DIR).join(INDEX_FILE_NAME)
    }

    fn ensure_loaded(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        let path = self.cache_path();
        let Ok(bytes) = fs::read(&path) else {
            return;
        };
        let Ok(Value::Object(root_obj)) = serde_json::from_slice::<Value>(&bytes) else {
            return;
        };
        if root_obj.get("version").and_then(Value::as_u64) != Some(INDEX_VERSION) {
            return;
        }
        let Some(Value::Object(map)) = root_obj.get("entries") else {
            return;
        };
        if map.len() > MAX_INDEX_ENTRIES {
            return;
        }
        let mut parsed = BTreeMap::new();
        for (rel, val) in map {
            if let Some(entry) = CachedFile::from_json(val) {
                parsed.insert(rel.clone(), entry);
            }
        }
        self.entries = parsed;
    }

    /// 命中有效缓存时返回该文件的符号列表（无需读盘）；未命中或指纹已变返回 `None`。
    pub(super) fn get_symbols(&mut self, rel: &str, stamp: FileStamp) -> Option<Vec<Symbol>> {
        self.ensure_loaded();
        let entry = self.entries.get(rel)?;
        if entry.stamp != stamp {
            return None;
        }
        Some(entry.symbols.iter().map(CachedSymbol::to_symbol).collect())
    }

    /// 更新某文件的符号缓存（若同指纹已有布隆过滤器则保留）。
    pub(super) fn put_symbols(&mut self, rel: String, stamp: FileStamp, syms: &[Symbol]) {
        self.ensure_loaded();
        let existing_bloom = self
            .entries
            .get(&rel)
            .filter(|e| e.stamp == stamp)
            .and_then(|e| e.ident_bloom);
        let cached = CachedFile {
            stamp,
            symbols: syms.iter().map(CachedSymbol::from_symbol).collect(),
            ident_bloom: existing_bloom,
        };
        self.insert_entry(rel, cached);
    }

    /// 更新某文件的符号与标识符布隆过滤器缓存（供 `find_references` 使用）。
    pub(super) fn put_symbols_and_bloom(
        &mut self,
        rel: String,
        stamp: FileStamp,
        syms: &[Symbol],
        bloom: [u64; 4],
    ) {
        self.ensure_loaded();
        let cached = CachedFile {
            stamp,
            symbols: syms.iter().map(CachedSymbol::from_symbol).collect(),
            ident_bloom: Some(bloom),
        };
        self.insert_entry(rel, cached);
    }

    /// 检查已缓存的标识符布隆过滤器：
    /// - `Some(false)`：指纹一致且布隆过滤器明确排除 `ident`（零假阴性，可安全跳过读盘）；
    /// - `Some(true)`：可能包含 `ident`，须读盘精查；
    /// - `None`：缓存未命中或尚未建立布隆过滤器，须读盘。
    pub(super) fn might_contain_ident(
        &mut self,
        rel: &str,
        stamp: FileStamp,
        ident: &str,
    ) -> Option<bool> {
        self.ensure_loaded();
        let entry = self.entries.get(rel)?;
        if entry.stamp != stamp {
            return None;
        }
        let bloom = entry.ident_bloom?;
        Some(bloom_might_contain(&bloom, ident))
    }

    fn insert_entry(&mut self, rel: String, entry: CachedFile) {
        if !self.entries.contains_key(&rel)
            && self.entries.len() >= MAX_INDEX_ENTRIES
            && let Some(first) = self.entries.keys().next().cloned()
        {
            self.entries.remove(&first);
        }
        self.entries.insert(rel, entry);
        self.dirty = true;
    }

    /// 若缓存有变更且项目根目录存在 `.ziyuan/`，原子落盘；任何 I/O 错误静默忽略（衍生缓存 fail-open）。
    pub(super) fn flush_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        let ziyuan_dir = self.root.join(".ziyuan");
        if !ziyuan_dir.is_dir() {
            self.dirty = false;
            return;
        }
        let index_dir = self.root.join(INDEX_REL_DIR);
        if fs::create_dir_all(&index_dir).is_err() {
            return;
        }
        let mut map = Map::with_capacity(self.entries.len());
        for (rel, entry) in &self.entries {
            map.insert(rel.clone(), entry.to_json());
        }
        let disk = json!({
            "version": INDEX_VERSION,
            "entries": Value::Object(map),
        });
        let Ok(bytes) = serde_json::to_vec(&disk) else {
            return;
        };
        static TMP_SEQ: AtomicU64 = AtomicU64::new(0);
        let tmp = index_dir.join(format!(
            "{INDEX_FILE_NAME}.tmp.{}.{}",
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        if fs::write(&tmp, bytes).is_err() {
            let _ = fs::remove_file(&tmp);
            return;
        }
        let target = self.cache_path();
        if fs::rename(&tmp, &target).is_err() {
            let _ = fs::remove_file(&target);
            if fs::rename(&tmp, &target).is_err() {
                let _ = fs::remove_file(&tmp);
                return;
            }
        }
        self.dirty = false;
    }
}

/// 基于已剔除注释与字符串的代码行构造 256 位标识符布隆过滤器（3 组 FNV-1a 派生哈希）。
pub(super) fn build_ident_bloom(stripped_lines: &[String]) -> [u64; 4] {
    let mut bloom = [0u64; 4];
    for line in stripped_lines {
        let mut start: Option<usize> = None;
        for (idx, ch) in line.char_indices() {
            if symbols::is_ref_ident_char(ch) {
                if start.is_none() {
                    start = Some(idx);
                }
            } else if let Some(s) = start.take() {
                bloom_insert(&mut bloom, &line[s..idx]);
            }
        }
        if let Some(s) = start {
            bloom_insert(&mut bloom, &line[s..]);
        }
    }
    bloom
}

fn fnv1a_seeded(bytes: &[u8], seed: u64) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64 ^ seed;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn bloom_bits(ident: &str) -> [usize; 3] {
    let b = ident.as_bytes();
    let h1 = fnv1a_seeded(b, 0x9e37_79b9_7f4a_7c15);
    let h2 = fnv1a_seeded(b, 0xc2b2_ae3d_27d4_eb4f);
    let h3 = h1.wrapping_add(h2.wrapping_mul(31));
    [
        (h1 & 0xff) as usize,
        (h2 & 0xff) as usize,
        (h3 & 0xff) as usize,
    ]
}

fn bloom_insert(bloom: &mut [u64; 4], ident: &str) {
    for bit in bloom_bits(ident) {
        bloom[bit / 64] |= 1u64 << (bit % 64);
    }
}

pub(super) fn bloom_might_contain(bloom: &[u64; 4], ident: &str) -> bool {
    bloom_bits(ident)
        .into_iter()
        .all(|bit| (bloom[bit / 64] & (1u64 << (bit % 64))) != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn temp_dir(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ziyuan-symindex-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".ziyuan")).unwrap();
        dir
    }

    #[test]
    fn bloom_has_zero_false_negatives_and_filters_absent_idents() {
        let lines = vec![
            "pub fn alpha_beta(gamma: u32) -> Delta {".to_owned(),
            "    epsilon_value + gamma".to_owned(),
            "}".to_owned(),
        ];
        let bloom = build_ident_bloom(&lines);
        for present in [
            "pub",
            "fn",
            "alpha_beta",
            "gamma",
            "u32",
            "Delta",
            "epsilon_value",
        ] {
            assert!(bloom_might_contain(&bloom, present), "missing {present}");
        }
        let absent_samples = [
            "completely_unrelated_one",
            "missing_identifier_two",
            "nonexistent_three",
            "zeta_four",
            "omega_five",
            "kappa_six",
        ];
        let rejected = absent_samples
            .iter()
            .filter(|s| !bloom_might_contain(&bloom, s))
            .count();
        assert!(
            rejected >= 5,
            "expected bloom to filter absent symbols, rejected {rejected}"
        );
    }

    #[test]
    fn index_persists_under_dot_ziyuan_invalidates_on_stamp_and_recovers_from_corruption() {
        let root = temp_dir("persist");
        let file = root.join("lib.rs");
        fs::write(
            &file,
            "pub fn hello() {}
",
        )
        .unwrap();
        let stamp1 = FileStamp::from_metadata(&fs::metadata(&file).unwrap()).unwrap();
        let (_, syms) = symbols::extract_for_path(
            Path::new("lib.rs"),
            "pub fn hello() {}
",
        )
        .unwrap();
        let stripped = symbols::strip_code_lines(
            Path::new("lib.rs"),
            "pub fn hello() {}
",
        );
        let bloom = build_ident_bloom(&stripped);

        {
            let mut idx = SymbolIndex::new(root.clone());
            assert!(idx.get_symbols("lib.rs", stamp1).is_none());
            idx.put_symbols_and_bloom("lib.rs".into(), stamp1, &syms, bloom);
            idx.flush_if_dirty();
        }
        let cache_file = root.join(".ziyuan/index/symbols-v1.json");
        assert!(cache_file.is_file(), "index file should be persisted");

        // Fresh instance hits disk cache
        {
            let mut idx = SymbolIndex::new(root.clone());
            let hit = idx.get_symbols("lib.rs", stamp1).expect("cache hit");
            assert_eq!(hit.len(), 1);
            assert_eq!(hit[0].name, "hello");
            assert_eq!(
                idx.might_contain_ident("lib.rs", stamp1, "hello"),
                Some(true)
            );
            assert_eq!(
                idx.might_contain_ident("lib.rs", stamp1, "definitely_not_in_file_xyz"),
                Some(false)
            );
        }

        // Changing file size immediately invalidates stamp
        fs::write(
            &file,
            "pub fn hello() {}
pub fn world() {}
",
        )
        .unwrap();
        let stamp2 = FileStamp::from_metadata(&fs::metadata(&file).unwrap()).unwrap();
        assert_ne!(stamp1, stamp2);
        {
            let mut idx = SymbolIndex::new(root.clone());
            assert!(idx.get_symbols("lib.rs", stamp2).is_none());
            assert!(idx.might_contain_ident("lib.rs", stamp2, "hello").is_none());
        }

        // Corrupt cache file fails open cleanly
        fs::write(&cache_file, b"{corrupt json").unwrap();
        {
            let mut idx = SymbolIndex::new(root.clone());
            assert!(idx.get_symbols("lib.rs", stamp1).is_none());
            idx.put_symbols("lib.rs".into(), stamp2, &syms);
            idx.flush_if_dirty();
            let parsed: Value = serde_json::from_slice(&fs::read(&cache_file).unwrap()).unwrap();
            assert_eq!(parsed["version"].as_u64(), Some(INDEX_VERSION));
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn index_does_not_create_dot_ziyuan_when_absent() {
        let dir =
            std::env::temp_dir().join(format!("ziyuan-symindex-nodot-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.rs");
        fs::write(
            &file,
            "fn x() {}
",
        )
        .unwrap();
        let stamp = FileStamp::from_metadata(&fs::metadata(&file).unwrap()).unwrap();
        let (_, syms) = symbols::extract_for_path(
            Path::new("a.rs"),
            "fn x() {}
",
        )
        .unwrap();

        let mut idx = SymbolIndex::new(dir.clone());
        idx.put_symbols("a.rs".into(), stamp, &syms);
        idx.flush_if_dirty();
        assert!(
            !dir.join(".ziyuan").exists(),
            "must not create .ziyuan in non-court dirs"
        );
        assert!(idx.get_symbols("a.rs", stamp).is_some());
        let _ = fs::remove_dir_all(dir);
    }
}
