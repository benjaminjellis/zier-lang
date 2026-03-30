use codespan_reporting::{diagnostic::Diagnostic, files::SimpleFiles};

use crate::{ast, lexer, lower, sexpr};

#[derive(Debug)]
pub struct HirModule {
    pub file_id: usize,
    pub files: SimpleFiles<String, String>,
    pub decls: Vec<ast::Declaration>,
    pub diagnostics: Vec<Diagnostic<usize>>,
}

pub fn lower_source_to_hir(source_path: &str, source: &str) -> HirModule {
    let mut lowerer = lower::Lowerer::new();
    let tokens = lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file(source_path.to_string(), source.to_string());

    let sexprs = match sexpr::SExprParser::new(tokens, file_id).parse() {
        Ok(sexprs) => sexprs,
        Err(diag) => {
            return HirModule {
                file_id,
                files: lowerer.files,
                decls: Vec::new(),
                diagnostics: vec![diag],
            };
        }
    };

    let decls = lowerer.lower_file(file_id, &sexprs);
    HirModule {
        file_id,
        files: lowerer.files,
        decls,
        diagnostics: lowerer.diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::lower_source_to_hir;

    #[test]
    fn lower_source_to_hir_returns_decls_for_valid_source() {
        let hir = lower_source_to_hir("main.mond", "(let main {} 1)");
        assert!(hir.diagnostics.is_empty());
        assert_eq!(hir.decls.len(), 1);
    }

    #[test]
    fn lower_source_to_hir_returns_diag_for_parse_error() {
        let hir = lower_source_to_hir("main.mond", "(let main {}");
        assert_eq!(hir.decls.len(), 0);
        assert!(!hir.diagnostics.is_empty());
    }
}
