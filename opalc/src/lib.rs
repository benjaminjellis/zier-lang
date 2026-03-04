use codespan_reporting::term::{
    self,
    termcolor::{ColorChoice, StandardStream},
};

pub mod ast;
pub mod ir;
pub mod lexer;
pub mod lower;
pub mod sexpr;
pub mod typecheck;

pub fn dummy_compile(source: &str) {
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    let writer = StandardStream::stderr(ColorChoice::Always);
    let config = codespan_reporting::term::Config::default();

    let file_id = lowerer.add_file("test.opal".to_string(), source.to_string());

    let sexprs = match crate::sexpr::SExprParser::new(tokens, file_id).parse() {
        Ok(res) => res,
        Err(diag) => {
            term::emit_to_write_style(&mut writer.lock(), &config, &lowerer.files, &diag).unwrap();
            return;
        }
    };

    let _ = lowerer.lower_file(file_id, &sexprs);

    for diag in &lowerer.diagnostics {
        term::emit_to_write_style(&mut writer.lock(), &config, &lowerer.files, diag).unwrap();
    }
}
