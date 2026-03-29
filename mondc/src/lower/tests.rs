use super::*;

// Helper to setup the lowerer with a string
fn setup(source: &str) -> (Lowerer, usize, Vec<SExpr>) {
    let mut lowerer = Lowerer::new();

    let tokens = crate::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file("test.mond".to_string(), source.to_string());

    // This assumes your Parser returns a Vec<SExpr>
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("S-Expr parse failed");

    (lowerer, file_id, sexprs)
}

#[test]
fn test_variant_type() {
    let (mut lowerer, file_id, sexprs) = setup(
        r#"(type ['a] Option [
                        None
                        (Some ~ 'a)])
                    "#,
    );

    let _exprs = lowerer.lower_file(file_id, &sexprs);
}

#[test]
fn test_record_type_with_generics() {
    let (mut lowerer, file_id, sexprs) = setup(
        "
                (type ['t] MyGenericType [
                    (:name ~ String)
                    (:data ~ 't)
                ])",
    );

    let exprs = lowerer.lower_file(file_id, &sexprs);
    if let Declaration::Type(TypeDecl::Record {
        name,
        params,
        fields,
        ..
    }) = &exprs[0]
    {
        assert_eq!(name, "MyGenericType");
        assert_eq!(params, &vec!["'t".to_string()]);
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].0, "name");
        assert!(matches!(&fields[0].1, TypeUsage::Named(name, _) if name == "String"));
        assert_eq!(fields[1].0, "data");
        assert!(matches!(&fields[1].1, TypeUsage::Generic(name, _) if name == "'t"));
    } else {
        panic!("expected a generic record type");
    }
}

#[test]
fn test_record_type_with_nested_type_application() {
    let (mut lowerer, file_id, sexprs) = setup(
        "
                (extern type ['p] Selector)
                (type ['m] ContinuePayload [
                    (:select ~ (Selector (Option 'm)))
                ])",
    );

    let exprs = lowerer.lower_file(file_id, &sexprs);
    if let Declaration::Type(TypeDecl::Record { fields, .. }) = &exprs[1] {
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].0, "select");
        assert!(matches!(
            &fields[0].1,
            TypeUsage::App(selector, args, _)
                if selector == "Selector"
                    && args.len() == 1
                    && matches!(
                        &args[0],
                        TypeUsage::App(option, option_args, _)
                            if option == "Option"
                                && option_args.len() == 1
                                && matches!(&option_args[0], TypeUsage::Generic(name, _) if name == "'m")
                    )
        ));
    } else {
        panic!("expected nested type application record type");
    }
}

#[test]
fn test_record_type_with_function_field() {
    let (mut lowerer, file_id, sexprs) = setup(
        "
                (type ['m] Builder [
                    (:initialised ~ ((Subject 'm) -> Unit))
                ])",
    );

    let exprs = lowerer.lower_file(file_id, &sexprs);
    if let Declaration::Type(TypeDecl::Record { fields, .. }) = &exprs[0] {
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].0, "initialised");
        assert!(matches!(
            &fields[0].1,
            TypeUsage::Fun(arg, ret, _)
                if matches!(
                    arg.as_ref(),
                    TypeUsage::App(subject, args, _)
                        if subject == "Subject"
                            && args.len() == 1
                            && matches!(&args[0], TypeUsage::Generic(name, _) if name == "'m")
                )
                && matches!(ret.as_ref(), TypeUsage::Named(name, _) if name == "Unit")
        ));
    } else {
        panic!("expected record type with function field");
    }
}

#[test]
fn test_record_type_with_empty_parenthesized_type_reports_error() {
    let (mut lowerer, file_id, sexprs) = setup(
        "
                (type Broken [
                    (:value ~ ())
                ])",
    );

    let decls = lowerer.lower_file(file_id, &sexprs);
    assert!(decls.is_empty(), "expected lowering to fail");
    assert!(!lowerer.diagnostics.is_empty());
    assert!(
        lowerer
            .diagnostics
            .iter()
            .any(|d| d.message.contains("empty type is not allowed"))
    );
}

#[test]
fn test_record_type() {
    let (mut lowerer, file_id, sexprs) = setup(
        "(type MyType [
                        (:field_one ~ String)
                        (:field_two ~ Int)
                        (:field_three ~ Bool)
                        ])",
    );

    let exprs = lowerer.lower_file(file_id, &sexprs);
    if let Declaration::Type(TypeDecl::Record {
        name,
        params,
        fields,
        ..
    }) = &exprs[0]
    {
        assert_eq!(name, "MyType");
        assert_eq!(*params, Vec::<String>::new());
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].0, "field_one");
        assert!(matches!(&fields[0].1, TypeUsage::Named(name, _) if name == "String"));
        assert_eq!(fields[1].0, "field_two");
        assert!(matches!(&fields[1].1, TypeUsage::Named(name, _) if name == "Int"));
        assert_eq!(fields[2].0, "field_three");
        assert!(matches!(&fields[2].1, TypeUsage::Named(name, _) if name == "Bool"));
    } else {
        panic!("expected a type not an expression")
    }
}

#[test]
fn test_record_type_square_bracket_body() {
    let (mut lowerer, file_id, sexprs) =
        setup("(pub type ExitMessage [(:pid ~ Pid) (:reason ~ ExitReason)])");

    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
    if let Declaration::Type(TypeDecl::Record {
        is_pub,
        name,
        params,
        fields,
        ..
    }) = &exprs[0]
    {
        assert!(*is_pub);
        assert_eq!(name, "ExitMessage");
        assert!(params.is_empty());
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].0, "pid");
        assert!(matches!(&fields[0].1, TypeUsage::Named(name, _) if name == "Pid"));
        assert_eq!(fields[1].0, "reason");
        assert!(matches!(&fields[1].1, TypeUsage::Named(name, _) if name == "ExitReason"));
    } else {
        panic!("expected a record type declaration");
    }
}

#[test]
fn test_lower_function() {
    let (mut lowerer, file_id, sexprs) = setup("(let f {a} (+ a 10))");
    let exprs = lowerer.lower_file(file_id, &sexprs);

    if let Declaration::Expression(Expr::LetFunc {
        name, args, value, ..
    }) = &exprs[0]
    {
        assert_eq!(name, "f");
        assert_eq!(args, &vec!["a".to_string()]);

        if let Expr::Call {
            func,
            args: call_args,
            ..
        } = &**value
        {
            if let Expr::Variable(op_name, _) = &**func {
                assert_eq!(op_name, "+");
            } else {
                panic!("Expected function call to be an operator variable '+'");
            }
            assert_eq!(call_args.len(), 2);
            assert!(matches!(call_args[0], Expr::Variable(ref n, _) if n == "a"));
            assert!(matches!(call_args[1], Expr::Literal(Literal::Int(10), _)));
        } else {
            panic!("Expected Let value to be a function call (+ ...)");
        }
    } else {
        panic!("Expected a Let expression at the top level");
    }
}

#[test]
fn test_let_sequential_desugaring() {
    // local let bindings are expressions — test via lower_expr, not lower_file
    let (mut lowerer, file_id, sexprs) = setup("(let [a 10 b 20] (+ a b))");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");

    // Should desugar to: LetLocal(a, 10, LetLocal(b, 20, Call(+, [a, b])))
    if let Expr::LetLocal { name, body, .. } = expr {
        assert_eq!(name, "a");
        if let Expr::LetLocal { name: name2, .. } = *body {
            assert_eq!(name2, "b");
        } else {
            panic!("Expected nested LetLocal for 'b'");
        }
    } else {
        panic!("Expected LetLocal for 'a'");
    }
}

#[test]
fn test_valid_if() {
    let (mut lowerer, file_id, sexprs) = setup("(if True 1 2)");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");

    if let Expr::If {
        cond,
        then,
        els,
        span,
    } = expr
    {
        assert!(
            matches!(*cond, Expr::Literal(Literal::Bool(true), _)),
            "Condition should be True"
        );

        assert!(
            matches!(*then, Expr::Literal(Literal::Int(1), _)),
            "Then-branch should be 1"
        );

        assert!(
            matches!(*els, Expr::Literal(Literal::Int(2), _)),
            "Else-branch should be 2"
        );

        // 4. Verify the Span covers the whole (if ...)
        assert_eq!(span.start, 0);
        assert_eq!(span.end, 13);
    } else {
        panic!("Expected Expr::If");
    }
}

#[test]
fn test_valid_if_let_desugars_to_match() {
    let (mut lowerer, file_id, sexprs) = setup("(if let [(Some x) maybe] x 0)");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");

    if let Expr::Match { targets, arms, .. } = expr {
        assert_eq!(targets.len(), 1);
        assert!(matches!(targets[0], Expr::Variable(ref n, _) if n == "maybe"));
        assert_eq!(arms.len(), 2);

        match &arms[0].patterns[0] {
            Pattern::Constructor(name, args, _) => {
                assert_eq!(name, "Some");
                assert!(matches!(
                    args.first(),
                    Some(Pattern::Variable(name, _)) if name == "x"
                ));
            }
            other => panic!("expected constructor pattern in if-let arm, got {other:?}"),
        }

        assert!(matches!(arms[0].body, Expr::Variable(ref n, _) if n == "x"));
        assert!(matches!(arms[1].patterns[0], Pattern::Any(_)));
        assert!(matches!(arms[1].body, Expr::Literal(Literal::Int(0), _)));
    } else {
        panic!("Expected Expr::Match desugaring for if-let");
    }
}

#[test]
fn test_valid_if_let_legacy_syntax_desugars_to_match() {
    let (mut lowerer, file_id, sexprs) = setup("(if let (Some x) maybe x 0)");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");

    assert!(
        matches!(expr, Expr::Match { .. }),
        "Expected Expr::Match desugaring for legacy if-let syntax"
    );
}

#[test]
fn test_error_reporting_on_invalid_if() {
    // 'if' with missing else branch
    let (mut lowerer, file_id, sexprs) = setup("(if True 1)");
    let _ = lowerer.lower_expr(file_id, &sexprs[0]);

    assert!(!lowerer.diagnostics.is_empty());
    assert_eq!(
        lowerer.diagnostics[0].message,
        "wrong number of arguments for 'if'"
    );
}

#[test]
fn test_error_reporting_on_invalid_if_let_arity() {
    let (mut lowerer, file_id, sexprs) = setup("(if let [(Some x) maybe] x)");
    let _ = lowerer.lower_expr(file_id, &sexprs[0]);
    assert!(!lowerer.diagnostics.is_empty());
    assert_eq!(
        lowerer.diagnostics[0].message,
        "wrong number of arguments for 'if let'"
    );
}

#[test]
fn test_error_reporting_on_invalid_if_let_binding_shape() {
    let (mut lowerer, file_id, sexprs) = setup("(if let [(Some x)] x 0)");
    let _ = lowerer.lower_expr(file_id, &sexprs[0]);
    assert!(!lowerer.diagnostics.is_empty());
    assert_eq!(lowerer.diagnostics[0].message, "invalid if let binding");
}

#[test]
fn test_float_literal() {
    let (mut lowerer, file_id, sexprs) = setup("6.14");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    if let Expr::Literal(Literal::Float(f), _) = expr {
        assert!((f - 6.14).abs() < 1e-10);
    } else {
        panic!("expected Float literal");
    }
}

#[test]
fn test_string_literal() {
    let (mut lowerer, file_id, sexprs) = setup(r#""hello world""#);
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    if let Expr::Literal(Literal::String(s), _) = expr {
        assert_eq!(s, "hello world");
    } else {
        panic!("expected String literal");
    }
}

#[test]
fn test_unit_literal() {
    let (mut lowerer, file_id, sexprs) = setup("()");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    assert!(matches!(expr, Expr::Literal(Literal::Unit, _)));
}

#[test]
fn test_list_literal_lowering() {
    let (mut lowerer, file_id, sexprs) = setup("[1 2 3]");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    if let Expr::List(items, _) = expr {
        assert_eq!(items.len(), 3);
        assert!(matches!(items[0], Expr::Literal(Literal::Int(1), _)));
        assert!(matches!(items[1], Expr::Literal(Literal::Int(2), _)));
        assert!(matches!(items[2], Expr::Literal(Literal::Int(3), _)));
    } else {
        panic!("expected List expression");
    }
}

#[test]
fn test_field_access_lowering() {
    let (mut lowerer, file_id, sexprs) = setup("(:my_field some_record)");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    if let Expr::FieldAccess { field, record, .. } = expr {
        assert_eq!(field, "my_field");
        assert!(matches!(record.as_ref(), Expr::Variable(n, _) if n == "some_record"));
    } else {
        panic!("expected FieldAccess");
    }
}

#[test]
fn test_field_access_too_many_args() {
    let (mut lowerer, file_id, sexprs) = setup("(:x p q)");
    let result = lowerer.lower_expr(file_id, &sexprs[0]);
    assert!(result.is_none());
    assert!(!lowerer.diagnostics.is_empty());
}

#[test]
fn test_field_access_zero_args() {
    let (mut lowerer, file_id, sexprs) = setup("(:x)");
    let result = lowerer.lower_expr(file_id, &sexprs[0]);
    assert!(result.is_none());
    assert!(!lowerer.diagnostics.is_empty());
}

#[test]
fn test_record_update_lowering() {
    let (mut lowerer, file_id, sexprs) = setup("(with point :x 10 :y 20)");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    if let Expr::RecordUpdate {
        record, updates, ..
    } = expr
    {
        assert!(matches!(record.as_ref(), Expr::Variable(name, _) if name == "point"));
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].0, "x");
        assert_eq!(updates[1].0, "y");
        assert!(matches!(updates[0].1, Expr::Literal(Literal::Int(10), _)));
        assert!(matches!(updates[1].1, Expr::Literal(Literal::Int(20), _)));
    } else {
        panic!("expected RecordUpdate");
    }
}

#[test]
fn test_record_update_missing_value_is_error() {
    let (mut lowerer, file_id, sexprs) = setup("(with point :x)");
    let result = lowerer.lower_expr(file_id, &sexprs[0]);
    assert!(result.is_none());
    assert!(!lowerer.diagnostics.is_empty());
}

#[test]
fn test_bare_field_accessor_error() {
    // :field used as a standalone atom (not inside parens) should error
    let (mut lowerer, file_id, sexprs) = setup("(let f {r} :my_field)");
    let exprs = lowerer.lower_file(file_id, &sexprs);
    // The :my_field as a bare value in position should produce a diagnostic
    assert!(exprs.is_empty() || !lowerer.diagnostics.is_empty());
}

#[test]
fn test_non_recursive() {
    let (mut lowerer, file_id, sexprs) = setup(
        "(let fib {n}
  (if (or (= n 0) (= n 1))
    n
    (+ (fib (- n 1)) (fib (- n 2)))))
",
    );

    let _a = lowerer.lower_file(file_id, &sexprs);
    assert!(lowerer.diagnostics.is_empty());
}

#[test]
fn test_let_with_continuation_fails() {
    // (let x 42 x) is an invalid let binding
    let (mut lowerer, file_id, sexprs) = setup("(let x 42 x)");
    let _ = lowerer.lower_file(file_id, &sexprs);
    let diag = &lowerer.diagnostics[0];
    assert_eq!(diag.message, "invalid let syntax")
}

#[test]
fn test_match_wildcard_pattern() {
    let (mut lowerer, file_id, sexprs) = setup("(match x _ ~> 0)");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    if let Expr::Match { arms, .. } = expr {
        assert_eq!(arms.len(), 1);
        assert!(
            matches!(arms[0].patterns[0], Pattern::Any(_)),
            "expected Any pattern"
        );
    } else {
        panic!("expected Match");
    }
}

#[test]
fn test_match_literal_pattern() {
    let (mut lowerer, file_id, sexprs) = setup("(match x 0 ~> True 1 ~> False _ ~> False)");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    if let Expr::Match { arms, .. } = expr {
        assert_eq!(arms.len(), 3);
        assert!(matches!(
            arms[0].patterns[0],
            Pattern::Literal(Literal::Int(0), _)
        ));
        assert!(matches!(
            arms[1].patterns[0],
            Pattern::Literal(Literal::Int(1), _)
        ));
        assert!(matches!(arms[2].patterns[0], Pattern::Any(_)));
    } else {
        panic!("expected Match");
    }
}

#[test]
fn test_if_too_many_args() {
    let (mut lowerer, file_id, sexprs) = setup("(if True 1 2 3)");
    let _ = lowerer.lower_expr(file_id, &sexprs[0]);
    assert!(!lowerer.diagnostics.is_empty());
    assert_eq!(
        lowerer.diagnostics[0].message,
        "wrong number of arguments for 'if'"
    );
}

#[test]
fn test_variant_type_multiple_constructors() {
    let (mut lowerer, file_id, sexprs) = setup("(type ['a 'e] Result [(Ok ~ 'a) (Error ~ 'e)])");
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert_eq!(exprs.len(), 1);
    if let Declaration::Type(TypeDecl::Variant {
        name,
        params,
        constructors,
        ..
    }) = &exprs[0]
    {
        assert_eq!(name, "Result");
        assert_eq!(params, &vec!["'a".to_string(), "'e".to_string()]);
        assert_eq!(constructors.len(), 2);
        let (ok_name, ok_payload) = &constructors[0];
        assert_eq!(ok_name, "Ok");
        assert!(ok_payload.is_some());
        let (err_name, err_payload) = &constructors[1];
        assert_eq!(err_name, "Error");
        assert!(err_payload.is_some());
    } else {
        panic!("expected Variant type declaration");
    }
}

#[test]
fn test_variant_type_square_bracket_body() {
    let (mut lowerer, file_id, sexprs) =
        setup("(pub type ['a] ExitReason [Normal Killed (Abnormal ~ 'a)])");
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert_eq!(exprs.len(), 1);
    assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
    if let Declaration::Type(TypeDecl::Variant {
        is_pub,
        name,
        params,
        constructors,
        ..
    }) = &exprs[0]
    {
        assert!(*is_pub);
        assert_eq!(name, "ExitReason");
        assert_eq!(params, &vec!["'a".to_string()]);
        assert_eq!(constructors.len(), 3);
        assert_eq!(constructors[0], ("Normal".into(), None));
        assert_eq!(constructors[1], ("Killed".into(), None));
        assert_eq!(constructors[2].0, "Abnormal");
        assert!(matches!(
            &constructors[2].1,
            Some(TypeUsage::Generic(name, _)) if name == "'a"
        ));
    } else {
        panic!("expected Variant type declaration");
    }
}

#[test]
fn test_match_constructor_patterns() {
    let (mut lowerer, file_id, sexprs) = setup("(match x (Some y) ~> y None ~> 0)");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");

    if let Expr::Match { arms, .. } = expr {
        if let Pattern::Constructor(name, args, _) = &arms[0].patterns[0] {
            assert_eq!(name, "Some");
            assert_eq!(args.len(), 1);
        } else {
            panic!("Expected Constructor pattern");
        }
    } else {
        panic!("expected Match");
    }
}

// -------------------------------------------------------------------------
// Lowerer acceptance tests — valid syntax that must lower without errors
// -------------------------------------------------------------------------

#[test]
fn test_float_operators() {
    // All four float ops must lower without errors
    for src in [
        "(+. 1.0 2.0)",
        "(-. 3.0 1.0)",
        "(*. 2.0 3.0)",
        "(/. 6.0 2.0)",
    ] {
        let (mut lowerer, file_id, sexprs) = setup(src);
        let result = lowerer.lower_expr(file_id, &sexprs[0]);
        assert!(result.is_some(), "failed for: {src}");
        assert!(lowerer.diagnostics.is_empty(), "failed for: {src}");
    }
}

#[test]
fn test_or_and_in_call_position() {
    // `or` and `and` are operators callable in function position
    for src in ["(or True False)", "(and True False)"] {
        let (mut lowerer, file_id, sexprs) = setup(src);
        let result = lowerer.lower_expr(file_id, &sexprs[0]);
        assert!(result.is_some(), "failed for: {src}");
        assert!(
            lowerer.diagnostics.is_empty(),
            "diagnostics: {:?}",
            lowerer.diagnostics
        );
    }
}

#[test]
fn test_pipe_desugars_to_nested_unary_calls() {
    let (mut lowerer, file_id, sexprs) = setup("(|> x inc double)");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);

    if let Expr::Call { func, args, .. } = expr {
        assert!(matches!(*func, Expr::Variable(ref name, _) if name == "double"));
        assert_eq!(args.len(), 1);
        if let Expr::Call {
            func: inner_func,
            args: inner_args,
            ..
        } = &args[0]
        {
            assert!(matches!(inner_func.as_ref(), Expr::Variable(name, _) if name == "inc"));
            assert_eq!(inner_args.len(), 1);
            assert!(matches!(inner_args[0], Expr::Variable(ref name, _) if name == "x"));
        } else {
            panic!("expected nested call for first pipe step");
        }
    } else {
        panic!("expected Call");
    }
}

#[test]
fn test_qualified_ident_in_value_position_lowers_to_variable() {
    let (mut lowerer, file_id, sexprs) = setup("io/debug");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
    assert!(matches!(expr, Expr::Variable(ref name, _) if name == "io/debug"));
}

#[test]
fn test_pipe_desugars_qualified_step_to_qualified_variable_call() {
    let (mut lowerer, file_id, sexprs) = setup("(|> x io/debug)");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);

    if let Expr::Call { func, args, .. } = expr {
        assert!(matches!(*func, Expr::Variable(ref name, _) if name == "io/debug"));
        assert_eq!(args.len(), 1);
        assert!(matches!(args[0], Expr::Variable(ref name, _) if name == "x"));
    } else {
        panic!("expected Call");
    }
}

#[test]
fn test_pipe_accepts_partial_application_steps() {
    let (mut lowerer, file_id, sexprs) = setup("(|> 3 (add 1) (mul 2))");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);

    if let Expr::Call { func, args, .. } = expr {
        assert_eq!(args.len(), 1);
        assert!(matches!(*func, Expr::Call { .. }));
    } else {
        panic!("expected Call");
    }
}

#[test]
fn test_pipe_hole_inserts_value_at_hole_position() {
    let (mut lowerer, file_id, sexprs) = setup("(|> 3 (add 1 _))");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);

    if let Expr::Call { func, args, .. } = expr {
        assert!(matches!(*func, Expr::Variable(ref name, _) if name == "add"));
        assert_eq!(args.len(), 2);
        assert!(matches!(args[0], Expr::Literal(Literal::Int(1), _)));
        assert!(matches!(args[1], Expr::Literal(Literal::Int(3), _)));
    } else {
        panic!("expected hole-substituted call");
    }
}

#[test]
fn test_pipe_hole_can_follow_regular_pipe_step() {
    let (mut lowerer, file_id, sexprs) = setup("(|> 3 (add 1) (mul _ 2))");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);

    if let Expr::Call { func, args, .. } = expr {
        assert!(matches!(*func, Expr::Variable(ref name, _) if name == "mul"));
        assert_eq!(args.len(), 2);
        assert!(matches!(args[1], Expr::Literal(Literal::Int(2), _)));
        if let Expr::Call {
            func: previous_func,
            args: previous_args,
            ..
        } = &args[0]
        {
            assert!(matches!(previous_func.as_ref(), Expr::Call { .. }));
            assert_eq!(previous_args.len(), 1);
            assert!(matches!(
                previous_args[0],
                Expr::Literal(Literal::Int(3), _)
            ));
        } else {
            panic!("expected previous pipe stage in first arg");
        }
    } else {
        panic!("expected outer call");
    }
}

#[test]
fn test_pipe_hole_identity_step_returns_current_value() {
    let (mut lowerer, file_id, sexprs) = setup("(|> x _)");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
    assert!(matches!(expr, Expr::Variable(ref name, _) if name == "x"));
}

#[test]
fn test_pipe_step_rejects_multiple_holes() {
    let (mut lowerer, file_id, sexprs) = setup("(|> x (pair _ _))");
    let expr = lowerer.lower_expr(file_id, &sexprs[0]);
    assert!(expr.is_none(), "expected lowering to fail");
    assert!(
        lowerer.diagnostics.iter().any(|d| d
            .message
            .contains("pipeline step can contain at most one `_` placeholder")),
        "unexpected diagnostics: {:?}",
        lowerer
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_pipe_requires_a_step() {
    let (mut lowerer, file_id, sexprs) = setup("(|> x)");
    let expr = lowerer.lower_expr(file_id, &sexprs[0]);
    assert!(expr.is_none(), "expected lowering to fail");
    assert!(
        lowerer.diagnostics.iter().any(|d| d
            .message
            .contains("pipeline requires a value and at least one step")),
        "unexpected diagnostics: {:?}",
        lowerer
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_multi_arg_function_lowering() {
    let (mut lowerer, file_id, sexprs) = setup("(let add {a b c} (+ a (+ b c)))");
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(lowerer.diagnostics.is_empty());
    if let Declaration::Expression(Expr::LetFunc { name, args, .. }) = &exprs[0] {
        assert_eq!(name, "add");
        assert_eq!(args, &["a", "b", "c"]);
    } else {
        panic!("expected LetFunc");
    }
}

#[test]
fn test_function_implicit_sequencing() {
    // Multiple expressions in a function body desugar to nested LetLocal "_"
    let src = "(let f {x} (+ x 1) (+ x 2))";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let decls = lowerer.lower_file(file_id, &sexprs);
    assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
    if let Declaration::Expression(Expr::LetFunc { value, .. }) = &decls[0] {
        // Body should be LetLocal { name: "_", value: (+ x 1), body: (+ x 2) }
        assert!(matches!(value.as_ref(), Expr::LetLocal { name, .. } if name == "_"));
    } else {
        panic!("expected LetFunc");
    }
}

#[test]
fn test_constructor_followed_by_body_expr_is_error() {
    let src = "(let always_none {x} None x)";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let _ = lowerer.lower_file(file_id, &sexprs);
    assert!(
        !lowerer.diagnostics.is_empty(),
        "expected diagnostic for ambiguous constructor sequencing"
    );
    assert!(
        lowerer.diagnostics[0]
            .message
            .contains("constructor `None` cannot be followed"),
        "unexpected error message: {}",
        lowerer.diagnostics[0].message
    );
}

#[test]
fn test_let_body_implicit_sequencing() {
    // Multiple expressions in a let body desugar to nested LetLocal "_"
    let src = "(let f {} (let [x 1] (+ x 1) (+ x 2)))";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let decls = lowerer.lower_file(file_id, &sexprs);
    assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
    if let Declaration::Expression(Expr::LetFunc { value, .. }) = &decls[0] {
        // Outer let [x 1] binds x, body is sequenced
        if let Expr::LetLocal { name, body, .. } = value.as_ref() {
            assert_eq!(name, "x");
            // Body of x binding should be LetLocal "_" for the sequence
            assert!(matches!(body.as_ref(), Expr::LetLocal { name, .. } if name == "_"));
        } else {
            panic!("expected LetLocal for x binding");
        }
    } else {
        panic!("expected LetFunc");
    }
}

#[test]
fn test_let_binding_with_function_call_value() {
    // local let with a call as value — test via lower_expr
    let (mut lowerer, file_id, sexprs) = setup("(let [x (+ 1 2)] x)");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    assert!(lowerer.diagnostics.is_empty());
    assert!(matches!(expr, Expr::LetLocal { ref name, .. } if name == "x"));
}

#[test]
fn test_match_multiple_arms() {
    let src = "(match n 0 ~> False 1 ~> True _ ~> False)";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    assert!(lowerer.diagnostics.is_empty());
    if let Expr::Match { arms, .. } = expr {
        assert_eq!(arms.len(), 3);
    } else {
        panic!("expected Match");
    }
}

#[test]
fn test_match_guard_lowers_on_arm() {
    let src = "(match x (Some y) if (> y 0) ~> y _ ~> 0)";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
    if let Expr::Match { arms, .. } = expr {
        assert_eq!(arms.len(), 2);
        assert!(arms[0].guard.is_some(), "expected first arm guard");
        assert!(arms[1].guard.is_none(), "expected second arm without guard");
    } else {
        panic!("expected Match");
    }
}

#[test]
fn test_match_guard_requires_expression_after_if() {
    let src = "(match x (Some y) if ~> y _ ~> 0)";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let _ = lowerer.lower_expr(file_id, &sexprs[0]);
    assert!(!lowerer.diagnostics.is_empty());
    assert!(
        lowerer.diagnostics[0]
            .message
            .contains("missing guard expression after `if`"),
        "unexpected diagnostic: {}",
        lowerer.diagnostics[0].message
    );
}

#[test]
fn test_do_sequences_expressions() {
    let src = "(let f {} (do (g 1) (h 2) (i 3)))";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let decls = lowerer.lower_file(file_id, &sexprs);
    assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
    if let Declaration::Expression(Expr::LetFunc { value, .. }) = &decls[0] {
        // (do e1 e2 e3) ~> LetLocal("_", e1, LetLocal("_", e2, e3))
        assert!(matches!(value.as_ref(), Expr::LetLocal { name, .. } if name == "_"));
    } else {
        panic!("expected LetFunc");
    }
}

#[test]
fn test_do_single_expr_is_identity() {
    let src = "(let f {} (do (g 1)))";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let decls = lowerer.lower_file(file_id, &sexprs);
    assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
    if let Declaration::Expression(Expr::LetFunc { value, .. }) = &decls[0] {
        // (do e) ~> e — no wrapping
        assert!(!matches!(value.as_ref(), Expr::LetLocal { .. }));
    } else {
        panic!("expected LetFunc");
    }
}

#[test]
fn test_do_empty_is_error() {
    let src = "(let f {} (do))";
    let (mut lowerer, file_id, sexprs) = setup(src);
    lowerer.lower_file(file_id, &sexprs);
    assert!(!lowerer.diagnostics.is_empty());
}

#[test]
fn test_match_arm_multi_expr_suggests_do() {
    // Two call expressions in a match arm without `do` should produce an error
    // with a hint to use `do`.
    let src = "(let f {x} (match x _ ~> (g x) (h x)))";
    let (mut lowerer, file_id, sexprs) = setup(src);
    lowerer.lower_file(file_id, &sexprs);
    assert!(
        !lowerer.diagnostics.is_empty(),
        "expected an error for multi-expr match arm"
    );
    assert!(
        lowerer.diagnostics[0].message.contains("do"),
        "error should mention `do`: {}",
        lowerer.diagnostics[0].message
    );
}

#[test]
fn test_variable_pattern_in_match() {
    let src = "(match x n ~> n)";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    assert!(lowerer.diagnostics.is_empty());
    if let Expr::Match { arms, .. } = expr {
        assert!(matches!(arms[0].patterns[0], Pattern::Variable(ref s, _) if s == "n"));
    } else {
        panic!("expected Match");
    }
}

#[test]
fn test_bool_literal_patterns() {
    let src = "(match b True ~> 0 False ~> 1)";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    assert!(lowerer.diagnostics.is_empty());
    if let Expr::Match { arms, .. } = expr {
        assert!(matches!(
            arms[0].patterns[0],
            Pattern::Literal(Literal::Bool(true), _)
        ));
        assert!(matches!(
            arms[1].patterns[0],
            Pattern::Literal(Literal::Bool(false), _)
        ));
    } else {
        panic!("expected Match");
    }
}

#[test]
fn test_nullary_constructor_pattern() {
    // None as a bare constructor pattern (no payload)
    let src = "(match x None ~> 0 (Some v) ~> v)";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    assert!(lowerer.diagnostics.is_empty());
    if let Expr::Match { arms, .. } = expr {
        assert!(
            matches!(&arms[0].patterns[0], Pattern::Constructor(name, args, _) if name == "None" && args.is_empty())
        );
        assert!(
            matches!(&arms[1].patterns[0], Pattern::Constructor(name, args, _) if name == "Some" && args.len() == 1)
        );
    } else {
        panic!("expected Match");
    }
}

#[test]
fn test_record_pattern() {
    let src = "(match person (Person :name name :age age) ~> age)";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    assert!(lowerer.diagnostics.is_empty());
    if let Expr::Match { arms, .. } = expr {
        match &arms[0].patterns[0] {
            Pattern::Record { name, fields, .. } => {
                assert_eq!(name, "Person");
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0].0, "name");
                assert!(matches!(fields[0].1, Pattern::Variable(ref s, _) if s == "name"));
                assert_eq!(fields[1].0, "age");
                assert!(matches!(fields[1].1, Pattern::Variable(ref s, _) if s == "age"));
            }
            other => panic!("expected record pattern, got {other:?}"),
        }
    } else {
        panic!("expected Match");
    }
}

#[test]
fn test_singleton_list_pattern_lowers_to_cons_empty() {
    let src = "(match xs [t] ~> t)";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
    if let Expr::Match { arms, .. } = expr {
        match &arms[0].patterns[0] {
            Pattern::Cons(head, tail, _) => {
                assert!(matches!(head.as_ref(), Pattern::Variable(name, _) if name == "t"));
                assert!(matches!(tail.as_ref(), Pattern::EmptyList(_)));
            }
            other => panic!("expected cons pattern, got {other:?}"),
        }
    } else {
        panic!("expected Match");
    }
}

#[test]
fn test_multi_head_list_cons_pattern_lowers_to_nested_cons() {
    let src = "(match xs [h t | rest] ~> rest)";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
    if let Expr::Match { arms, .. } = expr {
        match &arms[0].patterns[0] {
            Pattern::Cons(first_head, first_tail, _) => {
                assert!(matches!(first_head.as_ref(), Pattern::Variable(name, _) if name == "h"));
                match first_tail.as_ref() {
                    Pattern::Cons(second_head, second_tail, _) => {
                        assert!(
                            matches!(second_head.as_ref(), Pattern::Variable(name, _) if name == "t")
                        );
                        assert!(
                            matches!(second_tail.as_ref(), Pattern::Variable(name, _) if name == "rest")
                        );
                    }
                    other => panic!("expected nested cons tail, got {other:?}"),
                }
            }
            other => panic!("expected cons pattern, got {other:?}"),
        }
    } else {
        panic!("expected Match");
    }
}

// -------------------------------------------------------------------------
// Or-pattern and multi-target match tests
// -------------------------------------------------------------------------

#[test]
fn test_or_pattern_lowers_to_pattern_or() {
    let src = "(match x 1 | 2 | 3 ~> True _ ~> False)";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    assert!(lowerer.diagnostics.is_empty());
    if let Expr::Match { targets, arms, .. } = expr {
        assert_eq!(targets.len(), 1);
        assert_eq!(arms.len(), 2);
        assert!(matches!(&arms[0].patterns[0], Pattern::Or(pats, _) if pats.len() == 3));
        assert!(matches!(&arms[1].patterns[0], Pattern::Any(_)));
    } else {
        panic!("expected Match");
    }
}

#[test]
fn test_match_or_keyword_separator_is_rejected() {
    let src = "(match x 1 or 2 ~> True _ ~> False)";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let expr = lowerer.lower_expr(file_id, &sexprs[0]);
    assert!(expr.is_none(), "expected lowering to fail");
    assert!(
        lowerer
            .diagnostics
            .iter()
            .any(|d| d.message.contains("invalid pattern")),
        "expected invalid pattern diagnostic, got: {:?}",
        lowerer
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_multi_target_match_lowers_two_targets() {
    let src = "(match x y 1 1 ~> True _ _ ~> False)";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    assert!(lowerer.diagnostics.is_empty());
    if let Expr::Match { targets, arms, .. } = expr {
        assert_eq!(targets.len(), 2);
        assert_eq!(arms.len(), 2);
        assert_eq!(arms[0].patterns.len(), 2);
        assert!(matches!(
            &arms[0].patterns[0],
            Pattern::Literal(Literal::Int(1), _)
        ));
        assert!(matches!(
            &arms[0].patterns[1],
            Pattern::Literal(Literal::Int(1), _)
        ));
        assert!(matches!(&arms[1].patterns[0], Pattern::Any(_)));
        assert!(matches!(&arms[1].patterns[1], Pattern::Any(_)));
    } else {
        panic!("expected Match");
    }
}

// -------------------------------------------------------------------------
// Visibility (pub) tests
// -------------------------------------------------------------------------

#[test]
fn test_pub_function_is_pub() {
    let (mut lowerer, file_id, sexprs) = setup("(pub let add {a b} (+ a b))");
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(lowerer.diagnostics.is_empty());
    if let Declaration::Expression(Expr::LetFunc { is_pub, name, .. }) = &exprs[0] {
        assert!(is_pub, "expected is_pub = true");
        assert_eq!(name, "add");
    } else {
        panic!("expected LetFunc");
    }
}

#[test]
fn test_private_function_is_not_pub() {
    let (mut lowerer, file_id, sexprs) = setup("(let add {a b} (+ a b))");
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(lowerer.diagnostics.is_empty());
    if let Declaration::Expression(Expr::LetFunc { is_pub, .. }) = &exprs[0] {
        assert!(!is_pub, "expected is_pub = false");
    } else {
        panic!("expected LetFunc");
    }
}

#[test]
fn test_pub_type_is_pub() {
    let (mut lowerer, file_id, sexprs) = setup("(pub type ['a] Option [None (Some ~ 'a)])");
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(lowerer.diagnostics.is_empty());
    if let Declaration::Type(TypeDecl::Variant { is_pub, name, .. }) = &exprs[0] {
        assert!(is_pub, "expected is_pub = true");
        assert_eq!(name, "Option");
    } else {
        panic!("expected Variant TypeDecl");
    }
}

#[test]
fn test_pub_record_type_is_pub() {
    let (mut lowerer, file_id, sexprs) = setup("(pub type Point [(:x ~ Int) (:y ~ Int)])");
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(lowerer.diagnostics.is_empty());
    if let Declaration::Type(TypeDecl::Record { is_pub, name, .. }) = &exprs[0] {
        assert!(is_pub, "expected is_pub = true");
        assert_eq!(name, "Point");
    } else {
        panic!("expected Record TypeDecl");
    }
}

#[test]
fn test_duplicate_record_field_rejected() {
    let src = "(type LotsOfFields [(:record ~ String) (:record ~ String)])";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let decls = lowerer.lower_file(file_id, &sexprs);
    assert!(decls.is_empty(), "expected lowering to fail");
    assert_eq!(lowerer.diagnostics.len(), 1);
    assert_eq!(
        lowerer.diagnostics[0].message,
        "duplicate record field `:record`"
    );
}

#[test]
fn test_duplicate_variant_constructor_rejected() {
    let src = "(type LotsOVariants [One One Two])";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let decls = lowerer.lower_file(file_id, &sexprs);
    assert!(decls.is_empty(), "expected lowering to fail");
    assert_eq!(lowerer.diagnostics.len(), 1);
    assert_eq!(
        lowerer.diagnostics[0].message,
        "duplicate variant constructor `One`"
    );
}

#[test]
fn test_extern_type_without_target_is_valid() {
    let (mut lowerer, file_id, sexprs) = setup("(pub extern type Pid)");
    let decls = lowerer.lower_file(file_id, &sexprs);
    assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
    match &decls[0] {
        Declaration::ExternType {
            is_pub,
            name,
            params,
            erlang_target,
            ..
        } => {
            assert!(*is_pub);
            assert_eq!(name, "Pid");
            assert!(params.is_empty());
            assert!(erlang_target.is_none());
        }
        _ => panic!("expected ExternType"),
    }
}

#[test]
fn test_extern_type_with_target_is_still_valid() {
    let (mut lowerer, file_id, sexprs) = setup("(pub extern type ['k 'v] Map maps/map)");
    let decls = lowerer.lower_file(file_id, &sexprs);
    assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
    match &decls[0] {
        Declaration::ExternType {
            name,
            params,
            erlang_target,
            ..
        } => {
            assert_eq!(name, "Map");
            assert_eq!(params, &vec!["'k".to_string(), "'v".to_string()]);
            assert_eq!(
                erlang_target.as_ref(),
                Some(&("maps".to_string(), "map".to_string()))
            );
        }
        _ => panic!("expected ExternType"),
    }
}

#[test]
fn test_extern_let_missing_fields_is_error_not_panic() {
    let (mut lowerer, file_id, sexprs) = setup("(extern let)");
    let decls = lowerer.lower_file(file_id, &sexprs);
    assert!(decls.is_empty(), "expected lowering to fail");
    assert!(!lowerer.diagnostics.is_empty());
    assert_eq!(
        lowerer.diagnostics[0].message,
        "invalid extern let declaration"
    );
}

// -------------------------------------------------------------------------
// Lowerer rejection tests — invalid syntax that must produce diagnostics
// -------------------------------------------------------------------------

#[test]
fn test_rec_keyword_produces_error() {
    // `rec` was removed from the language
    let (mut lowerer, file_id, sexprs) = setup("(let rec f {x} x)");
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(exprs.is_empty());
    dbg!(&lowerer.diagnostics[0].labels);
    assert_eq!(lowerer.diagnostics[0].message, "invalid let syntax");
}

#[test]
fn test_top_level_local_binding_rejected() {
    // (let [x 42] x) is not valid at the top level — only inside a function body
    let (mut lowerer, file_id, sexprs) = setup("(let [x 42] x)");
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(exprs.is_empty());
    assert_eq!(
        lowerer.diagnostics[0].message,
        "local let binding is not valid at the top level"
    );
}

#[test]
fn test_empty_let_is_error() {
    // (let) with nothing after it is invalid
    let (mut lowerer, file_id, sexprs) = setup("(let)");
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(exprs.is_empty());
    assert!(!lowerer.diagnostics.is_empty());
}

#[test]
fn test_let_value_binding_without_braces_is_error() {
    // (let x 42) — missing {} means this is invalid let syntax
    let (mut lowerer, file_id, sexprs) = setup("(let x 42)");
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(exprs.is_empty());
    assert_eq!(lowerer.diagnostics[0].message, "invalid let syntax");
}

#[test]
fn test_let_binding_odd_count_is_error() {
    // (let [x] body) — one name with no value
    let (mut lowerer, file_id, sexprs) = setup("(let [x] 0)");
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(exprs.is_empty());
    assert!(!lowerer.diagnostics.is_empty());
}

#[test]
fn test_let_bind_missing_body_is_error() {
    let (mut lowerer, file_id, sexprs) = setup("(let? [_ (Ok ())])");
    let result = lowerer.lower_expr(file_id, &sexprs[0]);
    assert!(result.is_none());
    assert!(!lowerer.diagnostics.is_empty());
    assert_eq!(
        lowerer.diagnostics[0].message,
        "let? requires a body expression"
    );
}

#[test]
fn test_let_bind_desugars_to_result_match() {
    let (mut lowerer, file_id, sexprs) = setup("(let? [x (safe)] (Ok x))");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("expected let? to lower");
    assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);

    match expr {
        Expr::Match { arms, .. } => {
            assert_eq!(arms.len(), 2);
            match &arms[0].patterns[0] {
                Pattern::Constructor(name, args, _) => {
                    assert_eq!(name, "Ok");
                    assert!(matches!(
                        args.first(),
                        Some(Pattern::Variable(n, _)) if n == "x"
                    ));
                }
                other => panic!("expected Ok constructor pattern, got {other:?}"),
            }

            match &arms[1].patterns[0] {
                Pattern::Constructor(name, args, _) => {
                    assert_eq!(name, "Error");
                    assert!(matches!(
                        args.first(),
                        Some(Pattern::Variable(n, _)) if n == "__letq_error"
                    ));
                }
                other => panic!("expected Error constructor pattern, got {other:?}"),
            }

            match &arms[1].body {
                Expr::Call { func, args, .. } => {
                    assert!(matches!(
                        func.as_ref(),
                        Expr::Variable(name, _) if name == "Error"
                    ));
                    assert!(matches!(
                        args.first(),
                        Some(Expr::Variable(name, _)) if name == "__letq_error"
                    ));
                }
                other => panic!("expected Error constructor call, got {other:?}"),
            }
        }
        other => panic!("expected let? to desugar to match, got {other:?}"),
    }
}

#[test]
fn test_match_with_no_arms_is_error() {
    // (match x) — no patterns at all
    let (mut lowerer, file_id, sexprs) = setup("(match x)");
    let result = lowerer.lower_expr(file_id, &sexprs[0]);
    assert!(result.is_none());
    assert!(!lowerer.diagnostics.is_empty());
}

#[test]
fn test_match_missing_arrow_is_error() {
    // (match x pat body) — missing ~>
    let (mut lowerer, file_id, sexprs) = setup("(match x 0 1)");
    let result = lowerer.lower_expr(file_id, &sexprs[0]);
    assert!(result.is_none());
    assert!(!lowerer.diagnostics.is_empty());
}

#[test]
fn test_match_missing_body_is_error() {
    // (match x 0 ~>) — arrow present but no result expression
    let (mut lowerer, file_id, sexprs) = setup("(match x 0 ~>)");
    let result = lowerer.lower_expr(file_id, &sexprs[0]);
    assert!(result.is_none());
    assert!(!lowerer.diagnostics.is_empty());
}

#[test]
fn test_let_function_missing_body_is_error() {
    // (let f {}) — args present but body missing
    let (mut lowerer, file_id, sexprs) = setup("(let f {})");
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(exprs.is_empty());
    assert!(!lowerer.diagnostics.is_empty());
}

#[test]
fn test_let_function_rejects_f_as_argument_name() {
    let (mut lowerer, file_id, sexprs) = setup("(let main {f} f)");
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(exprs.is_empty());
    assert!(!lowerer.diagnostics.is_empty());
    assert_eq!(
        lowerer.diagnostics[0].message,
        "invalid argument name, 'f' is a reserved keyword for anonymous functions"
    );
}

#[test]
fn test_lambda_rejects_f_as_argument_name() {
    let (mut lowerer, file_id, sexprs) = setup("(f {f} -> f)");
    let result = lowerer.lower_expr(file_id, &sexprs[0]);
    assert!(result.is_none());
    assert!(!lowerer.diagnostics.is_empty());
    assert_eq!(
        lowerer.diagnostics[0].message,
        "invalid argument name, 'f' is a reserved keyword for anonymous functions"
    );
}

#[test]
fn test_standalone_curly_is_error() {
    // {} as a top-level expression is invalid
    let (mut lowerer, file_id, sexprs) = setup("{x}");
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(exprs.is_empty());
    assert!(!lowerer.diagnostics.is_empty());
}

#[test]
fn test_list_literal_at_top_level_is_error() {
    // [1 2 3] at top level is not a valid declaration
    let (mut lowerer, file_id, sexprs) = setup("[1 2 3]");
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(exprs.is_empty());
    assert!(!lowerer.diagnostics.is_empty());
}

// -------------------------------------------------------------------------
// Variant type declaration rejection tests
// -------------------------------------------------------------------------

#[test]
fn test_variant_spurious_atom_rejected() {
    // `None x` — `x` is not a valid constructor name (lowercase)
    let src = "(type ['a] Option [None x (Some ~ 'a)])";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(exprs.is_empty(), "expected lowering to fail");
    assert!(!lowerer.diagnostics.is_empty());
    assert_eq!(
        lowerer.diagnostics[0].message,
        "invalid variant constructor"
    );
}

#[test]
fn test_variant_lowercase_constructor_rejected() {
    // Constructor names must start with uppercase
    let src = "(type ['a] Option [none (Some ~ 'a)])";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(exprs.is_empty(), "expected lowering to fail");
    assert_eq!(
        lowerer.diagnostics[0].message,
        "invalid variant constructor"
    );
}

#[test]
fn test_variant_constructor_missing_tilde_rejected() {
    // (Some 'a) instead of (Some ~ 'a)
    let src = "(type ['a] Option [None (Some 'a)])";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(exprs.is_empty(), "expected lowering to fail");
    assert!(!lowerer.diagnostics.is_empty());
}

#[test]
fn test_variant_square_payload_requires_parentheses() {
    // In square-body form, payload constructors still require parens:
    // [Normal Killed (Abnormal ~ 'a)]
    let src = "(type ['a] ExitReason [Normal Killed Abnormal ~ 'a])";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(exprs.is_empty(), "expected lowering to fail");
    assert!(!lowerer.diagnostics.is_empty());
    assert_eq!(
        lowerer.diagnostics[0].message,
        "invalid variant constructor"
    );
}

#[test]
fn test_variant_constructor_lowercase_name_in_payload_rejected() {
    // (some ~ 'a) — constructor name is lowercase
    let src = "(type ['a] Option [None (some ~ 'a)])";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(exprs.is_empty(), "expected lowering to fail");
    assert_eq!(
        lowerer.diagnostics[0].message,
        "constructor name must start with an uppercase letter"
    );
}

#[test]
fn test_variant_integer_in_body_rejected() {
    // A literal in the variant body is not a constructor
    let src = "(type Foo [Bar 42])";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(exprs.is_empty(), "expected lowering to fail");
    assert!(!lowerer.diagnostics.is_empty());
}

#[test]
fn test_type_params_require_quote_prefix() {
    let src = "(type ['a b] Pair [(:left ~ 'a) (:right ~ b)])";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(exprs.is_empty(), "expected lowering to fail");
    assert!(!lowerer.diagnostics.is_empty());
    assert_eq!(
        lowerer.diagnostics[0].message,
        "type parameters must start with `'`"
    );
}

#[test]
fn test_extern_type_params_require_quote_prefix() {
    let src = "(extern type ['k v] Dict maps/map)";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(exprs.is_empty(), "expected lowering to fail");
    assert!(!lowerer.diagnostics.is_empty());
    assert_eq!(
        lowerer.diagnostics[0].message,
        "type parameters must start with `'`"
    );
}

#[test]
fn test_type_round_body_rejected() {
    let src = "(type Flag (On Off))";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(exprs.is_empty(), "expected lowering to fail");
    assert!(!lowerer.diagnostics.is_empty());
    assert_eq!(
        lowerer.diagnostics[0].message,
        "type body must be wrapped in square brackets"
    );
}

#[test]
fn test_bare_expression_at_top_level_rejected() {
    let (mut lowerer, file_id, sexprs) = setup("42");
    let _ = lowerer.lower_file(file_id, &sexprs);
    assert!(!lowerer.diagnostics.is_empty());
    assert_eq!(
        lowerer.diagnostics[0].message,
        "only function and type declarations are valid at the top level"
    );
}

#[test]
fn test_bare_call_at_top_level_rejected() {
    let (mut lowerer, file_id, sexprs) = setup("(foo 1 2)");
    let _ = lowerer.lower_file(file_id, &sexprs);
    assert!(!lowerer.diagnostics.is_empty());
    assert_eq!(
        lowerer.diagnostics[0].message,
        "only function and type declarations are valid at the top level"
    );
}

#[test]
fn test_use_duplicate_import_name_rejected() {
    let src = "(use std/io [println println])";
    let (mut lowerer, file_id, sexprs) = setup(src);
    let exprs = lowerer.lower_file(file_id, &sexprs);
    assert!(exprs.is_empty(), "expected lowering to fail");
    assert!(!lowerer.diagnostics.is_empty());
    assert_eq!(lowerer.diagnostics[0].message, "duplicate import in list");
}
