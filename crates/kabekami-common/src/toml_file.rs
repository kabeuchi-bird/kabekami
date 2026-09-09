//! 壊れていても致命傷にしない TOML 読み込み。
//!
//! kabekami には「読めなければ既定値で動き続ける」種類の TOML ファイルが
//! 複数ある（`state.toml`、言語ファイル）。ユーザーがうっかり壊しても
//! デーモンが起動しなくなるのは望ましくないため、読み込み失敗を
//! 警告に留めて `None` を返す。
//!
//! `config.toml` はこれを使わない。設定が壊れている場合はユーザーに
//! はっきり伝える必要があるため、`Config::load` は意図的にエラーを
//! 呼び出し元へ伝播させている。

use std::path::Path;

use serde::de::DeserializeOwned;

/// TOML ファイルを読み、失敗したら警告を出して `None` を返す。
///
/// - ファイルが無い場合は警告を出さない（未作成は正常な状態）
/// - 読み込みエラー・パースエラーは `what` を添えて警告する
///
/// `what` はログに出す対象名（例: `"state file"`, `"language file"`）。
pub fn load_lenient<T: DeserializeOwned>(path: &Path, what: &str) -> Option<T> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!("cannot read {} {}: {}", what, path.display(), e);
            return None;
        }
    };
    match toml::from_str(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!("malformed {} {}, ignored: {}", what, path.display(), e);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct Sample {
        value: u32,
    }

    #[test]
    fn reads_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.toml");
        std::fs::write(&p, "value = 7").unwrap();
        assert_eq!(load_lenient::<Sample>(&p, "sample"), Some(Sample { value: 7 }));
    }

    #[test]
    fn missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nope.toml");
        assert_eq!(load_lenient::<Sample>(&p, "sample"), None);
    }

    #[test]
    fn malformed_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.toml");
        std::fs::write(&p, "this is not = valid = toml").unwrap();
        assert_eq!(load_lenient::<Sample>(&p, "sample"), None);
    }

    /// 型が合わない場合もパースエラーとして扱う。
    #[test]
    fn type_mismatch_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("wrong.toml");
        std::fs::write(&p, "value = \"not a number\"").unwrap();
        assert_eq!(load_lenient::<Sample>(&p, "sample"), None);
    }
}
