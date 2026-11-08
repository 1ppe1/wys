//! ソース位置（FileId, Span, SourceMap）

/// 0始まりの UTF-8 バイト範囲。`end` は含まない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: u32, end: u32) -> Self {
        assert!(start <= end, "span start must not exceed end");
        Self { start, end }
    }
}

/// 1始まりの行・列（列は Unicode スカラー値単位）と、0始まりのバイトオフセット。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub column: u32,
    pub byte: u32,
}

/// 1ファイル分のソース。バイトオフセットから行・列への変換を行う。
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub name: String,
    pub text: String,
    line_starts: Vec<u32>,
}

impl SourceFile {
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> Self {
        let text = text.into();
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(u32::try_from(i + 1).expect("source file too large"));
            }
        }
        Self {
            name: name.into(),
            text,
            line_starts,
        }
    }

    /// バイトオフセットを行・列に変換する。文字境界でないオフセットはパニックする。
    pub fn position(&self, byte: u32) -> Position {
        let line_idx = match self.line_starts.binary_search(&byte) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let line_start = self.line_starts[line_idx] as usize;
        let column = self.text[line_start..byte as usize].chars().count() + 1;
        Position {
            line: line_idx as u32 + 1,
            column: column as u32,
            byte,
        }
    }
}

/// 複数のソースファイルを1本のバイト位置で扱う（モジュール、ADR-0010）。
/// 各ファイルに基準位置（base）を割り当て、`Span` はその通し番号で表す。
#[derive(Debug, Clone, Default)]
pub struct SourceMap {
    files: Vec<(u32, SourceFile)>,
}

impl SourceMap {
    pub fn single(file: SourceFile) -> Self {
        let mut m = Self::default();
        m.add(file);
        m
    }

    /// ファイルを追加し、その基準位置を返す。ファイルどうしの位置が重ならないよう、1バイトの隙間を空ける。
    pub fn add(&mut self, file: SourceFile) -> u32 {
        let base = match self.files.last() {
            Some((b, f)) => b + u32::try_from(f.text.len()).expect("source file too large") + 1,
            None => 0,
        };
        self.files.push((base, file));
        base
    }

    fn find(&self, byte: u32) -> (u32, &SourceFile) {
        let i = self.files.partition_point(|(b, _)| *b <= byte);
        let (base, file) = &self.files[i.max(1) - 1];
        (*base, file)
    }

    /// 位置 `byte` を含むファイルの名前。
    pub fn file_name(&self, byte: u32) -> &str {
        &self.find(byte).1.name
    }

    /// 位置 `byte` の行・列（`Position::byte` はファイル内の位置）。
    pub fn position(&self, byte: u32) -> Position {
        let (base, file) = self.find(byte);
        file.position(byte - base)
    }

    /// 名前が `matches` に合う最初のファイルの基準位置。
    pub fn base_of_file(&self, matches: impl Fn(&str) -> bool) -> Option<u32> {
        self.files
            .iter()
            .find(|(_, f)| matches(&f.name))
            .map(|(b, _)| *b)
    }

    /// 位置 `byte` を含むファイル。
    pub fn file_at(&self, byte: u32) -> &SourceFile {
        self.find(byte).1
    }

    /// 最初に追加したファイル（エントリファイル）。
    pub fn entry(&self) -> &SourceFile {
        &self.files[0].1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_map_locates_files_and_positions() {
        let mut m = SourceMap::default();
        let a = m.add(SourceFile::new("a.wys", "x = 1\n"));
        let b = m.add(SourceFile::new("b.wys", "y\nz = 2\n"));
        assert_eq!(a, 0);
        assert_eq!(b, 7);
        assert_eq!(m.file_name(2), "a.wys");
        assert_eq!(m.file_name(b + 2), "b.wys");
        let p = m.position(b + 2);
        assert_eq!((p.line, p.column, p.byte), (2, 1, 2));
    }

    #[test]
    fn positions_are_one_based_and_count_chars() {
        let f = SourceFile::new("a.wys", "x = 1\ny = \"あい\"\n");
        assert_eq!(
            f.position(0),
            Position {
                line: 1,
                column: 1,
                byte: 0
            }
        );
        // 2行目の先頭
        assert_eq!(f.position(6).line, 2);
        assert_eq!(f.position(6).column, 1);
        // "あ" は3バイトだが1列として数える
        let after_a = 6 + "y = \"あ".len() as u32;
        assert_eq!(f.position(after_a).column, 7);
    }

    #[test]
    #[should_panic]
    fn span_rejects_reversed_range() {
        Span::new(3, 1);
    }
}
