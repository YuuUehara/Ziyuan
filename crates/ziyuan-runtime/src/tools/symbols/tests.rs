use super::*;

#[test]
fn rust_extracts_items_impl_methods_and_skips_comments_and_raw_strings() {
    let src = r####"
// fn commented_out() {}
/*
  struct InBlockComment;
  /* nested fn nested_comment() {} */
  enum StillComment {}
*/
pub const SAMPLE: &str = r#"
pub fn inside_raw_string() {}
"#;

pub mod parser {
    pub struct Config {
        pub limit: usize,
    }

    pub trait Runner {
        fn run(&self) -> bool;
    }

    impl Runner for Config {
        fn run(&self) -> bool {
            fn local_helper() {}
            true
        }
    }

    pub(crate) async unsafe fn serve<'a>(cfg: &'a Config) {}
}

macro_rules! quick {
    () => {};
}
"####;
    let syms = extract(Language::Rust, src);
    let summary: Vec<_> = syms.iter().map(|s| (s.kind, s.qualified_name())).collect();
    assert_eq!(
        summary,
        vec![
            ("const", "SAMPLE".into()),
            ("mod", "parser".into()),
            ("struct", "mod parser > Config".into()),
            ("trait", "mod parser > Runner".into()),
            ("fn", "trait Runner > run".into()),
            ("impl", "mod parser > Runner for Config".into()),
            ("fn", "impl Runner for Config > run".into()),
            ("fn", "mod parser > serve".into()),
            ("macro", "quick".into()),
        ]
    );
}

#[test]
fn jsts_python_go_and_markdown_extract_clean_symbols() {
    let js = r#"
// function nope() {}
export class Editor {
    async save(doc) {
        return doc;
    }
}
export const renderView = (x) => x;
export function helper() {}
"#;
    let js_syms: Vec<_> = extract(Language::JsTs, js)
        .into_iter()
        .map(|s| (s.kind, s.qualified_name()))
        .collect();
    assert_eq!(
        js_syms,
        vec![
            ("class", "Editor".into()),
            ("fn", "class Editor > save".into()),
            ("fn", "renderView".into()),
            ("fn", "helper".into()),
        ]
    );

    let py = r#""""
def inside_docstring():
    pass
"""
class Service:
    async def fetch(self):
        def inner():
            pass
        return 1

def top_level():
    pass
"#;
    let py_syms: Vec<_> = extract(Language::Python, py)
        .into_iter()
        .map(|s| (s.kind, s.qualified_name()))
        .collect();
    assert_eq!(
        py_syms,
        vec![
            ("class", "Service".into()),
            ("fn", "class Service > fetch".into()),
            ("fn", "top_level".into()),
        ]
    );

    let go = r#"
package demo
type Server struct{}
func (s *Server) Start() error { return nil }
func New() *Server { return nil }
"#;
    let go_syms: Vec<_> = extract(Language::Go, go)
        .into_iter()
        .map(|s| (s.kind, s.qualified_name()))
        .collect();
    assert_eq!(
        go_syms,
        vec![
            ("struct", "Server".into()),
            ("fn", "Server > Start".into()),
            ("fn", "New".into()),
        ]
    );

    let md = "# Title\n```md\n# Not A Heading\n```\n## Section\n";
    let md_syms: Vec<_> = extract(Language::Markdown, md)
        .into_iter()
        .map(|s| (s.kind, s.name, s.depth))
        .collect();
    assert_eq!(
        md_syms,
        vec![
            ("heading", "Title".into(), 0),
            ("heading", "Section".into(), 1),
        ]
    );
}

#[test]
fn symbol_end_lines_cover_all_supported_languages() {
    let rs = r####"pub const SAMPLE: &str = r#"
line 2
"#;

pub mod m {
    pub struct Cfg {
        pub limit: usize,
    }

    pub fn multi(
        a: i32,
    ) -> i32 {
        a + 1
    }
}

macro_rules! quick {
    () => {};
}
"####;
    let rs_spans: Vec<_> = extract(Language::Rust, rs)
        .into_iter()
        .map(|s| (s.name, s.line, s.end_line))
        .collect();
    assert_eq!(
        rs_spans,
        vec![
            ("SAMPLE".into(), 1, 3),
            ("m".into(), 5, 15),
            ("Cfg".into(), 6, 8),
            ("multi".into(), 10, 14),
            ("quick".into(), 17, 19),
        ]
    );

    let js = "\
export class Editor {
    async save(doc) {
        return doc;
    }
}
export const renderView = (x) => x;
export function helper(a) {
    return a;
}
";
    let js_spans: Vec<_> = extract(Language::JsTs, js)
        .into_iter()
        .map(|s| (s.name, s.line, s.end_line))
        .collect();
    assert_eq!(
        js_spans,
        vec![
            ("Editor".into(), 1, 5),
            ("save".into(), 2, 4),
            ("renderView".into(), 6, 6),
            ("helper".into(), 7, 9),
        ]
    );

    let py = "\
class Service:
    \"\"\"doc\"\"\"
    async def fetch(self):
        def inner():
            pass
        return 1

# next function
def top_level():
    \"\"\"only doc\"\"\"
";
    let py_spans: Vec<_> = extract(Language::Python, py)
        .into_iter()
        .map(|s| (s.name, s.line, s.end_line))
        .collect();
    assert_eq!(
        py_spans,
        vec![
            ("Service".into(), 1, 6),
            ("fetch".into(), 3, 6),
            ("top_level".into(), 9, 10),
        ]
    );

    let go = "\
package demo
type Server struct {
    port int
}
func (s *Server) Start(
    ctx int,
) error {
    return nil
}
const Max = 10
";
    let go_spans: Vec<_> = extract(Language::Go, go)
        .into_iter()
        .map(|s| (s.name, s.line, s.end_line))
        .collect();
    assert_eq!(
        go_spans,
        vec![
            ("Server".into(), 2, 4),
            ("Start".into(), 5, 9),
            ("Max".into(), 10, 10),
        ]
    );

    let cpp = "\
#define ADD(a, b) \\
    ((a) + (b))
int compute(int x)
{
    return ADD(x, 1);
}
";
    let cpp_spans: Vec<_> = extract(Language::CCpp, cpp)
        .into_iter()
        .map(|s| (s.name, s.line, s.end_line))
        .collect();
    assert_eq!(
        cpp_spans,
        vec![("ADD".into(), 1, 2), ("compute".into(), 3, 6)]
    );

    let md = "\
# Title
intro

## First
body 1

## Second
body 2
";
    let md_spans: Vec<_> = extract(Language::Markdown, md)
        .into_iter()
        .map(|s| (s.name, s.line, s.end_line))
        .collect();
    assert_eq!(
        md_spans,
        vec![
            ("Title".into(), 1, 8),
            ("First".into(), 4, 5),
            ("Second".into(), 7, 8),
        ]
    );
}
