use std::sync::Arc as Rc;

use super::*;

fn check_with_env(src: &str, extra_env: TypeEnv) -> Result<Rc<Type>, TypeError> {
    let tokens = crate::lexer::Lexer::new(src).lex();
    let mut lowerer = crate::lower::Lowerer::new();

    let file_id = lowerer.add_file("test.mond".into(), src.into());

    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse failed");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let mut checker = TypeChecker::new();
    let mut env = primitive_env();
    env.extend(extra_env);

    checker
        .check_program(&mut env, &decls, file_id)
        .map_err(|err| err.0)
}

fn check(src: &str) -> Result<Rc<Type>, TypeError> {
    let tokens = crate::lexer::Lexer::new(src).lex();
    let mut lowerer = crate::lower::Lowerer::new();

    let file_id = lowerer.add_file("test.mond".into(), src.into());

    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse failed");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let mut checker = TypeChecker::new();
    let mut env = primitive_env();

    checker
        .check_program(&mut env, &decls, file_id)
        .map_err(|err| err.0)
}

fn check_and_env(src: &str) -> Result<TypeEnv, TypeError> {
    let tokens = crate::lexer::Lexer::new(src).lex();
    let mut lowerer = crate::lower::Lowerer::new();

    let file_id = lowerer.add_file("test.mond".into(), src.into());

    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse failed");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let mut checker = TypeChecker::new();
    let mut env = primitive_env();
    checker
        .check_program(&mut env, &decls, file_id)
        .map_err(|err| err.0)?;
    Ok(env)
}

fn check_expr(src: &str) -> Result<Rc<Type>, TypeError> {
    let tokens = crate::lexer::Lexer::new(src).lex();
    let mut lowerer = crate::lower::Lowerer::new();
    let file_id = lowerer.add_file("test.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse failed");
    let expr = lowerer
        .lower_expr(file_id, &sexprs[0])
        .expect("lowering failed");
    let mut checker = TypeChecker::new();
    let env = primitive_env();
    checker.infer(&env, &expr).map(|(_, ty, _)| ty)
}

#[test]
fn infer_int_literal() {
    let ty = check_expr("42").unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn type_display_renders_curried_functions_without_extra_parens() {
    let ty = Type::fun(Type::int(), Type::fun(Type::int(), Type::int()));
    assert_eq!(type_display(&ty), "Int -> Int -> Int");
}

#[test]
fn type_display_keeps_parens_for_function_arguments() {
    let ty = Type::fun(Type::fun(Type::int(), Type::int()), Type::int());
    assert_eq!(type_display(&ty), "(Int -> Int) -> Int");
}

#[test]
fn predicate_display_renders_hasfield_constraint() {
    let pred = Predicate::HasField {
        label: "selector".to_string(),
        record_ty: Rc::new(Type::Var(4_200)),
        field_ty: Type::int(),
    };
    assert_eq!(predicate_display(&pred), "HasField :selector 'a Int");
}

#[test]
fn scheme_display_renders_qualified_types() {
    let var = 4_200_u64;
    let scheme = Scheme {
        vars: vec![var],
        preds: vec![Predicate::HasField {
            label: "selector".to_string(),
            record_ty: Rc::new(Type::Var(var)),
            field_ty: Type::int(),
        }],
        ty: Type::fun(Rc::new(Type::Var(var)), Type::int()),
    };
    assert_eq!(
        scheme_display(&scheme),
        "HasField :selector 'a Int => 'a -> Int"
    );
}

#[test]
fn unsatisfied_field_constraint_diagnostic_renders_full_predicate() {
    let err = TypeError::UnsatisfiedFieldConstraint {
        field: "selector".to_string(),
        record_ty: Type::con("Initialised", vec![]),
        field_ty: Type::bool(),
        candidates: vec!["ContinuePayload".to_string()],
    };
    let diags = err.to_diagnostics(0, 0..0);
    assert!(
        diags[0]
            .message
            .contains("HasField :selector Initialised Bool"),
        "unexpected diagnostic message: {}",
        diags[0].message
    );
}

#[test]
fn infer_bool_literal() {
    let ty = check_expr("True").unwrap();
    assert_eq!(ty, Type::bool());
}

#[test]
fn infer_arithmetic() {
    let ty = check_expr("(+ 1 2)").unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_if() {
    let ty = check_expr("(if True 1 2)").unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_let_function() {
    let ty = check("(let double {x} (* 2 x))\n(let main {} (double 5))").unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_let_binding() {
    // Local bindings live inside function bodies; use a 1-arg wrapper
    let ty = check("(let helper {dummy} (let [x 42] x))\n(let main {} (helper 0))").unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_identity_is_polymorphic() {
    // Use identity at two different types to verify polymorphism.
    let src = "(let id {x} x)\n(let get_a {dummy} (let [a (id 42) b (id True)] a))\n(let main {} (get_a 0))";
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_lambda_binding_is_polymorphic_under_value_restriction() {
    let src = "(let get_id {dummy} (let [id (f {x} -> x) a (id 42) b (id True)] a))\n(let main {} (get_id 0))";
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_constructor_binding_is_polymorphic_under_value_restriction() {
    let src = "(type ['a] Option [None (Some ~ 'a)])\n(let get_none {dummy} (let [none None a none b none] a))\n(let main {} (get_none 0))";
    let ty = check(src).unwrap();
    match ty.as_ref() {
        Type::Con(name, args) => {
            assert_eq!(name, "Option");
            assert_eq!(args.len(), 1);
        }
        _ => panic!("expected Con(Option, _), got {:?}", ty),
    }
}

#[test]
fn infer_expansive_process_subject_binding_is_monomorphic() {
    let mut env = TypeEnv::new();
    let subject_var = 10_000;
    env.insert(
        "process/new_subject".into(),
        Scheme {
            vars: vec![subject_var],
            preds: vec![],
            ty: Type::fun(
                Type::unit(),
                Type::con("Subject", vec![Rc::new(Type::Var(subject_var))]),
            ),
        },
    );
    env.insert(
        "process/send".into(),
        Scheme {
            vars: vec![subject_var],
            preds: vec![],
            ty: Type::fun(
                Type::con("Subject", vec![Rc::new(Type::Var(subject_var))]),
                Type::fun(
                    Rc::new(Type::Var(subject_var)),
                    Rc::new(Type::Var(subject_var)),
                ),
            ),
        },
    );

    let src = "(let main {} (let [subject (process/new_subject) a (process/send subject \"hello\") b (process/send subject 10)] a))";
    let err = check_with_env(src, env).expect_err("expected monomorphic subject binding");
    assert!(matches!(err, TypeError::Mismatch { .. }));
}

#[test]
fn unbound_variable_error() {
    let tokens = crate::lexer::Lexer::new("x").lex();

    let mut lowerer = crate::lower::Lowerer::new();
    let file_id = lowerer.add_file("test.mond".into(), "x".into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .unwrap();
    let expr = lowerer.lower_expr(file_id, &sexprs[0]).unwrap();

    let mut checker = TypeChecker::new();
    let env = primitive_env();
    assert!(matches!(
        checker.infer(&env, &expr),
        Err(TypeError::UnboundVariable(_, _))
    ));
}

#[test]
fn type_mismatch_error() {
    // (+ True 1) should fail
    let tokens = crate::lexer::Lexer::new("(+ True 1)").lex();

    let mut lowerer = crate::lower::Lowerer::new();
    let file_id = lowerer.add_file("test.mond".into(), "(+ True 1)".into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .unwrap();
    let expr = lowerer.lower_expr(file_id, &sexprs[0]).unwrap();

    let mut checker = TypeChecker::new();
    let env = primitive_env();
    assert!(matches!(
        checker.infer(&env, &expr),
        Err(TypeError::Mismatch { .. })
    ));
}

#[test]
fn nullary_constructor_call_has_helpful_diagnostic() {
    let src = "(type ['a] Option [None (Some ~ 'a)])\n(let always_none {x} (None x))";
    let tokens = crate::lexer::Lexer::new(src).lex();
    let mut lowerer = crate::lower::Lowerer::new();
    let file_id = lowerer.add_file("test.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse failed");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let mut checker = TypeChecker::new();
    let mut env = primitive_env();
    let err = checker
        .check_program(&mut env, &decls, file_id)
        .expect_err("expected type error");
    let (type_err, expr) = *err;
    let diags = type_err.to_diagnostics(file_id, expr.span());

    assert!(
        diags[0].message.contains("cannot call non-function `None`"),
        "unexpected message: {}",
        diags[0].message
    );
    assert!(
        diags[0]
            .notes
            .iter()
            .any(|n| n.contains("nullary constructor")),
        "expected nullary constructor hint in notes: {:?}",
        diags[0].notes
    );
}

#[test]
fn unknown_field_diagnostic_has_no_visibility_hint_by_default() {
    let err = TypeError::UnknownField {
        field: "name".into(),
        record_ty: Type::con("Dir", vec![]),
        field_span: 0..4,
        def: None,
    };
    let diags = err.to_diagnostics(0, 0..4);

    assert!(
        diags[0].notes.is_empty(),
        "did not expect visibility notes for plain unknown-field errors, got: {:?}",
        diags[0].notes
    );
}

#[test]
fn inaccessible_private_record_field_diagnostic_is_explicit() {
    let err = TypeError::InaccessiblePrivateRecordField {
        field: "name".into(),
        record: "Dir".into(),
        modules: vec!["fs".into()],
        field_span: 0..1,
    };
    let diags = err.to_diagnostics(0, 0..1);

    assert!(
        diags[0].message.contains("private record `Dir`"),
        "expected explicit private-record message, got: {}",
        diags[0].message
    );
    assert!(
        diags[0]
            .notes
            .iter()
            .any(|n| n.contains("(pub type Dir [...])")),
        "expected concrete pub-type export instruction, got notes: {:?}",
        diags[0].notes,
    );
}

#[test]
fn call_mismatch_mentions_inferred_callee_type() {
    let src = r#"
            (type ['a 'e] Result [(Ok ~ 'a) (Error ~ 'e)])
            (extern let println ~ (String -> Unit) io/format)
            (let match_and_print {val}
              (match val
                (Ok x) ~> (println x)
                (Error err) ~> (println err)))
            (let main {}
              (let [good (Ok "hello")
                    bad  (Error ())]
                (do (match_and_print good)
                    (match_and_print bad))))
        "#;
    let tokens = crate::lexer::Lexer::new(src).lex();
    let mut lowerer = crate::lower::Lowerer::new();
    let file_id = lowerer.add_file("test.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse failed");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let mut checker = TypeChecker::new();
    let mut env = primitive_env();
    let err = checker
        .check_program(&mut env, &decls, file_id)
        .expect_err("expected type error");
    let (type_err, expr) = *err;
    let diags = type_err.to_diagnostics(file_id, expr.span());

    assert!(
        diags[0]
            .labels
            .iter()
            .any(|label| label.message.contains("`match_and_print` was inferred as")),
        "expected inferred callee type label, got labels: {:?}",
        diags[0].labels
    );
    assert!(
        diags[0].labels.iter().any(|label| label
            .message
            .contains("`match_and_print` expects `Result String String` here")),
        "expected full argument type label, got labels: {:?}",
        diags[0].labels
    );
}

#[test]
fn infer_option_none() {
    let src = r#"
            (type ['a] Option [None (Some ~ 'a)])
            (let get_none {} None)
        "#;
    let ty = check(src).unwrap();
    // get_none : Option<'a> — should be Con("Option", [_])
    match ty.as_ref() {
        Type::Con(name, args) => {
            assert_eq!(name, "Option");
            assert_eq!(args.len(), 1);
        }
        _ => panic!("expected Con(Option, _), got {:?}", ty),
    }
}

#[test]
fn infer_option_some() {
    let src = r#"
            (type ['a] Option [None (Some ~ 'a)])
            (let get_some {} (Some 42))
        "#;
    let ty = check(src).unwrap();
    // Some 42 : Option<Int>
    assert_eq!(ty, Type::con("Option", vec![Type::int()]));
}

#[test]
fn infer_match_option() {
    let src = r#"
            (type ['a] Option [None (Some ~ 'a)])
            (let safe_add {opt n}
              (match opt
                None ~> 0
                (Some x) ~> (+ x n)))
            (let main {} (safe_add (Some 5) 3))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_record_construction() {
    let src = r#"
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let main {} (Point :x 0 :y 0))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::con("Point", vec![]));
}

#[test]
fn infer_record_construction_accepts_out_of_order_named_fields() {
    let src = r#"
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let main {} (Point :y 0 :x 0))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::con("Point", vec![]));
}

#[test]
fn infer_record_construction_rejects_missing_fields() {
    let src = r#"
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let main {} (Point :x 0))
        "#;
    let result = check(src);
    assert!(
        matches!(
            result,
            Err(TypeError::MissingRecordFields {
                ref record,
                ref missing,
                ..
            })
                if record == "Point" && missing == &vec!["y".to_string()]
        ),
        "expected MissingRecordFields for Point.y, got {result:?}"
    );
}

#[test]
fn missing_record_fields_diagnostic_highlights_constructor_expression() {
    let src = r#"
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let main {} (let [x 1] (Point :x 0)))
        "#;
    let tokens = crate::lexer::Lexer::new(src).lex();
    let mut lowerer = crate::lower::Lowerer::new();
    let file_id = lowerer.add_file("test.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse failed");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let mut checker = TypeChecker::new();
    let mut env = primitive_env();
    let err = checker
        .check_program(&mut env, &decls, file_id)
        .expect_err("expected missing field error");
    let (type_err, top_level_expr) = *err;
    let diags = type_err.to_diagnostics(file_id, top_level_expr.span());
    let primary = diags[0].labels.first().expect("primary label");
    let expected_start = src.find("(Point :x 0)").expect("constructor start");
    let expected_end = expected_start + "(Point :x 0)".len();

    assert_eq!(
        primary.range,
        expected_start..expected_end,
        "missing-field diagnostic should point at the record constructor expression"
    );
}

#[test]
fn infer_record_construction_rejects_duplicate_fields() {
    let src = r#"
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let main {} (Point :x 0 :x 1 :y 2))
        "#;
    let result = check(src);
    assert!(
        matches!(
            result,
            Err(TypeError::DuplicateRecordField {
                ref record,
                ref field,
                ..
            }) if record == "Point" && field == "x"
        ),
        "expected DuplicateRecordField for Point.x, got {result:?}"
    );
}

#[test]
fn infer_record_construction_rejects_unknown_field() {
    let src = r#"
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let main {} (Point :x 0 :z 1))
        "#;
    let result = check(src);
    assert!(
        matches!(result, Err(TypeError::UnknownField { ref field, .. }) if field == "z"),
        "expected UnknownField for z, got {result:?}"
    );
}

#[test]
fn infer_record_update() {
    let src = r#"
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let main {} (with (Point :x 0 :y 1) :x 10))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::con("Point", vec![]));
}

#[test]
fn infer_record_update_can_change_generic_field_type() {
    let src = r#"
            (type ['s 'r] Initialised [(:state ~ 's) (:return ~ 'r)])
            (let initialised {state} (Initialised :state state :return ()))
            (let returning {result initialised} (with initialised :return result))
            (let main {} (returning 1 (initialised True)))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(
        ty,
        Type::con("Initialised", vec![Type::bool(), Type::int()])
    );
}

#[test]
fn infer_record_update_rejects_unknown_field() {
    let src = r#"
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let main {} (with (Point :x 0 :y 1) :z 10))
        "#;
    let result = check(src);
    assert!(
        matches!(result, Err(TypeError::UnknownField { ref field, .. }) if field == "z"),
        "expected UnknownField for z, got {result:?}"
    );
}

#[test]
fn infer_record_update_rejects_duplicate_fields() {
    let src = r#"
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let main {} (with (Point :x 0 :y 1) :x 10 :x 20))
        "#;
    let result = check(src);
    assert!(
        matches!(
            result,
            Err(TypeError::DuplicateRecordUpdateField { ref field, .. }) if field == "x"
        ),
        "expected DuplicateRecordUpdateField for x, got {result:?}"
    );
}

#[test]
fn infer_generic_record_construction() {
    let src = r#"
            (type ['t] Box [(:value ~ 't)])
            (let main {} (Box :value 42))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::con("Box", vec![Type::int()]));
}

#[test]
fn infer_record_construction_field_type_error() {
    let src = r#"
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let main {} (Point :x True :y 0))
        "#;
    assert!(
        check(src).is_err(),
        "expected type error: Bool used for Int field"
    );
}

#[test]
fn infer_record_function_field_and_call() {
    let src = r#"
            (type FnBox [(:run ~ (Int -> String))])
            (let main {} ((:run (FnBox :run (f {n} -> "ok"))) 1))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::string());
}

#[test]
fn infer_lambda_identity() {
    let ty = check_expr("(f {x} -> x)").unwrap();
    // Should be 'a -> 'a
    match ty.as_ref() {
        Type::Fun(a, b) => assert_eq!(a, b),
        _ => panic!("expected Fun type, got {ty:?}"),
    }
}

#[test]
fn infer_nullary_lambda_as_unit_function() {
    let ty = check_expr("(f {} -> ())").unwrap();
    assert_eq!(ty, Type::fun(Type::unit(), Type::unit()));
}

#[test]
fn infer_lambda_applied() {
    // Immediately apply a lambda
    let ty = check_expr("((f {x} -> (+ x 1)) 5)").unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_lambda_as_arg() {
    let src = r#"
            (let apply {func x} (func x))
            (let main {} (apply (f {n} -> (* n 2)) 3))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_nullary_lambda_as_arg() {
    let src = r#"
            (let run {task} (task))
            (let main {} (run (f {} -> ())))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::unit());
}

#[test]
fn infer_nullary_named_function_as_unit_function() {
    let env = check_and_env("(let my_func {} ())").unwrap();
    let scheme = env
        .get("my_func")
        .expect("expected `my_func` to be present in type environment");
    assert_eq!(scheme.ty, Type::fun(Type::unit(), Type::unit()));
}

#[test]
fn infer_nullary_named_function_last_type_is_body_type() {
    let ty = check("(let my_func {} ())").unwrap();
    assert_eq!(ty, Type::unit());
}

#[test]
fn infer_let_bind() {
    // let? is built-in Result short-circuiting sugar.
    let src = r#"
            (type ['a 'e] Result [
                (Ok ~ 'a)
                (Error ~ 'e)])
            (let safe_inc {r}
                (let? [x r]
                    (Ok (+ x 1))))
            (let main {} (safe_inc (Ok 41)))
        "#;
    let ty = check(src).unwrap();
    // Error type is polymorphic (never constrained); only check the success type
    match ty.as_ref() {
        Type::Con(name, args) if name == "Result" => assert_eq!(args[0], Type::int()),
        _ => panic!("expected Result type, got {ty:?}"),
    }
}

#[test]
fn instantiate_freshens_scheme_predicates_with_type_vars() {
    let mut checker = TypeChecker::new();
    let scheme_var = 9_999_u64;
    let scheme = Scheme {
        vars: vec![scheme_var],
        preds: vec![Predicate::HasField {
            label: "state".to_string(),
            record_ty: Rc::new(Type::Var(scheme_var)),
            field_ty: Type::int(),
        }],
        ty: Type::fun(Rc::new(Type::Var(scheme_var)), Type::int()),
    };

    let (ty, preds) = checker.instantiate_with_preds(&scheme);
    let instantiated_arg = match ty.as_ref() {
        Type::Fun(arg, _) => match arg.as_ref() {
            Type::Var(id) => *id,
            other => panic!("expected instantiated argument var, got {other:?}"),
        },
        other => panic!("expected function type, got {other:?}"),
    };
    let pred_record = match preds.first() {
        Some(Predicate::HasField { record_ty, .. }) => match record_ty.as_ref() {
            Type::Var(id) => *id,
            other => panic!("expected predicate record var, got {other:?}"),
        },
        other => panic!("expected first predicate to be HasField, got {other:?}"),
    };

    assert_eq!(instantiated_arg, pred_record);
    assert_ne!(instantiated_arg, scheme_var);
}

#[test]
fn top_level_field_access_binding_discharges_concrete_hasfield_predicate() {
    let src = r#"
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let get_x {p} (:x p))
        "#;
    let env = check_and_env(src).unwrap();
    let scheme = env
        .get("get_x")
        .expect("expected `get_x` to be present in type environment");
    assert_eq!(
        scheme.ty,
        Type::fun(Type::con("Point", vec![]), Type::int())
    );
    assert!(
        scheme.preds.is_empty(),
        "concrete field access should discharge HasField constraints, got {:?}",
        scheme.preds
    );
}

#[test]
fn top_level_record_update_binding_discharges_concrete_hasfield_predicate() {
    let src = r#"
            (type ['s 'r] Initialised [(:state ~ 's) (:return ~ 'r)])
            (let returning {result initialised} (with initialised :return result))
        "#;
    let env = check_and_env(src).unwrap();
    let scheme = env
        .get("returning")
        .expect("expected `returning` to be present in type environment");
    assert!(
        scheme.preds.is_empty(),
        "concrete record update should discharge HasField constraints, got {:?}",
        scheme.preds
    );
}

#[test]
fn top_level_field_access_binding_retains_polymorphic_hasfield_predicate() {
    let src = r#"
            (type ContinuePayload [(:selector ~ Int)])
            (type Initialised [(:selector ~ Bool)])
            (let read_selector {x} (:selector x))
        "#;
    let env = check_and_env(src).unwrap();
    let scheme = env
        .get("read_selector")
        .expect("expected `read_selector` to be present in type environment");

    match (scheme.ty.as_ref(), scheme.preds.first()) {
        (
            Type::Fun(arg_ty, ret_ty),
            Some(Predicate::HasField {
                label,
                record_ty,
                field_ty,
            }),
        ) => {
            assert_eq!(label, "selector");
            assert_eq!(arg_ty.as_ref(), record_ty.as_ref());
            assert_eq!(ret_ty.as_ref(), field_ty.as_ref());
        }
        other => panic!("expected constrained selector accessor scheme, got {other:?}"),
    }
}

#[test]
fn call_solves_imported_hasfield_constraint() {
    let mut extra_env = TypeEnv::new();
    let record_var = 11_001_u64;
    let field_var = 11_002_u64;
    extra_env.insert(
        "get_state".into(),
        Scheme {
            vars: vec![record_var, field_var],
            preds: vec![Predicate::HasField {
                label: "state".to_string(),
                record_ty: Rc::new(Type::Var(record_var)),
                field_ty: Rc::new(Type::Var(field_var)),
            }],
            ty: Type::fun(
                Rc::new(Type::Var(record_var)),
                Rc::new(Type::Var(field_var)),
            ),
        },
    );

    let src = r#"
            (type ContinuePayload [(:state ~ Int)])
            (let main {} (get_state (ContinuePayload :state 1)))
        "#;
    let ty = check_with_env(src, extra_env).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn call_reports_unsatisfied_hasfield_constraint() {
    let mut extra_env = TypeEnv::new();
    let record_var = 12_001_u64;
    let field_var = 12_002_u64;
    extra_env.insert(
        "get_selector".into(),
        Scheme {
            vars: vec![record_var, field_var],
            preds: vec![Predicate::HasField {
                label: "selector".to_string(),
                record_ty: Rc::new(Type::Var(record_var)),
                field_ty: Rc::new(Type::Var(field_var)),
            }],
            ty: Type::fun(
                Rc::new(Type::Var(record_var)),
                Rc::new(Type::Var(field_var)),
            ),
        },
    );

    let src = r#"
            (type ContinuePayload [(:state ~ Int)])
            (let main {} (get_selector (ContinuePayload :state 1)))
        "#;
    let err = check_with_env(src, extra_env).expect_err("expected unsatisfied HasField");
    assert!(
        matches!(
            err,
            TypeError::UnsatisfiedFieldConstraint { ref field, .. } if field == "selector"
        ),
        "expected UnsatisfiedFieldConstraint for :selector, got {err:?}"
    );
}

#[test]
fn infer_field_access() {
    let src = r#"
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let get_x {p} (:x p))
            (let main {} (get_x (Point :x 5 :y 10)))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_field_access_generic_record() {
    let src = r#"
            (type ['t] Box [(:value ~ 't)])
            (let get_val {b} (:value b))
            (let main {} (get_val (Box :value 42)))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_field_access_is_disambiguated_by_callsite_record_type() {
    let src = r#"
            (type ContinuePayload [(:selector ~ Int)])
            (type Initialised [(:selector ~ Bool)])
            (let read_selector {x} (:selector x))
            (let main {} (read_selector (ContinuePayload :selector 1)))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_result_type() {
    let src = r#"
            (type ['a 'e] Result [(Ok ~ 'a) (Error ~ 'e)])
            (let identity {r} r)
            (let main {} (identity (Ok 42)))
        "#;
    let ty = check(src).unwrap();
    // Should be Con("Result", [Con("Int", []), _])
    match ty.as_ref() {
        Type::Con(name, args) => {
            assert_eq!(name, "Result");
            assert_eq!(args.len(), 2);
            assert_eq!(args[0], Type::int());
        }
        _ => panic!("expected Con(Result, _), got {:?}", ty),
    }
}

#[test]
fn infer_recursive_fib() {
    // Named functions are self-recursive by default — no `rec` needed
    let src = r#"
            (let fib {n}
              (if (= n 0)
                0
                (if (= n 1)
                  1
                  (+ (fib (- n 1)) (fib (- n 2))))))
            (let main {} (fib 10))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn field_access_wrong_type() {
    let src = r#"
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let test {} (:x 42))
        "#;
    let result = check(src);
    assert!(result.is_err(), "expected type error, got {:?}", result);
}

#[test]
fn infer_float_literal() {
    let ty = check_expr("3.14").unwrap();
    assert_eq!(ty, Type::float());
}

#[test]
fn infer_string_literal() {
    let ty = check_expr(r#""hello world""#).unwrap();
    assert_eq!(ty, Type::string());
}

#[test]
fn infer_float_arithmetic() {
    let ty = check_expr("(+. 1.5 2.5)").unwrap();
    assert_eq!(ty, Type::float());
}

#[test]
fn infer_list_of_ints() {
    let ty = check_expr("[1 2 3]").unwrap();
    assert_eq!(ty, Type::array(Type::int()));
}

#[test]
fn infer_empty_list() {
    let ty = check_expr("[]").unwrap();
    // empty list has a polymorphic element type var
    match ty.as_ref() {
        Type::Con(name, args) => {
            assert_eq!(name, "List");
            assert_eq!(args.len(), 1);
            assert!(matches!(args[0].as_ref(), Type::Var(_)));
        }
        _ => panic!("expected List type, got {:?}", ty),
    }
}

#[test]
fn infer_list_type_mismatch() {
    // Int and Bool in the same list must fail
    let result = check_expr("[1 True]");
    assert!(result.is_err(), "expected type error, got {:?}", result);
}

#[test]
fn infer_boolean_and() {
    let ty = check_expr("(and True False)").unwrap();
    assert_eq!(ty, Type::bool());
}

#[test]
fn infer_not() {
    let ty = check_expr("(not True)").unwrap();
    assert_eq!(ty, Type::bool());
}

#[test]
fn infer_comparison_lt() {
    let ty = check_expr("(< 1 2)").unwrap();
    assert_eq!(ty, Type::bool());
}

#[test]
fn infer_equality_polymorphic() {
    // = works on Int
    let ty = check_expr("(= 1 1)").unwrap();
    assert_eq!(ty, Type::bool());
    // = also works on Bool
    let ty2 = check_expr("(= True False)").unwrap();
    assert_eq!(ty2, Type::bool());
}

#[test]
fn infer_if_non_bool_condition() {
    // Condition must be Bool; Int should fail
    let result = check_expr("(if 1 2 3)");
    assert!(result.is_err(), "expected type error, got {:?}", result);
    assert!(
        matches!(result, Err(TypeError::ConditionNotBool { .. })),
        "expected ConditionNotBool"
    );
}

#[test]
fn infer_if_branch_type_mismatch() {
    // then=Int, else=Bool → branches must agree
    let result = check_expr("(if True 1 True)");
    assert!(result.is_err(), "expected type error, got {:?}", result);
}

#[test]
fn infer_multi_arg_function() {
    let ty = check("(let add {a b} (+ a b))\n(let main {} (add 1 2))").unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_wildcard_pattern() {
    let src = r#"
            (type ['a] Option [None (Some ~ 'a)])
            (let is_some {opt}
              (match opt
                None ~> False
                _ ~> True))
            (let main {} (is_some (Some 1)))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::bool());
}

#[test]
fn infer_literal_pattern_in_match() {
    // Match on integer literals
    let src = "(let classify {n} (match n 0 ~> True _ ~> False))\n(let main {} (classify 0))";
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::bool());
}

#[test]
fn infer_occurs_check() {
    // (let recur {x} (recur recur)) — calling recur with itself causes a -> b = a, infinite type
    let result = check("(let recur {x} (recur recur))");
    assert!(
        result.is_err(),
        "expected type error for infinite type, got {:?}",
        result
    );
}

#[test]
fn infer_higher_order_function() {
    // apply : (a -> b) -> a -> b applied to double
    let src = r#"
            (let apply {func x} (func x))
            (let double {n} (* 2 n))
            (let main {} (apply double 5))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_chained_field_access() {
    let src = r#"
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let sum_coords {p} (+ (:x p) (:y p)))
            (let main {} (sum_coords (Point 3 4)))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_nullary_variant_in_match() {
    let src = r#"
            (type ['a] Option [None (Some ~ 'a)])
            (let identity {x} x)
            (let main {} (identity None))
        "#;
    let ty = check(src).unwrap();
    match ty.as_ref() {
        Type::Con(name, args) => {
            assert_eq!(name, "Option");
            assert_eq!(args.len(), 1);
            // The type arg should be a type variable
            assert!(matches!(args[0].as_ref(), Type::Var(_)));
        }
        _ => panic!("expected Con(Option, [Var(_)]), got {:?}", ty),
    }
}

// -------------------------------------------------------------------------
// Typecheck acceptance tests — valid programs that must type-check
// -------------------------------------------------------------------------

#[test]
fn infer_self_recursive_without_rec() {
    // Named functions are self-recursive — no `rec` needed
    let src = "(let sum {n} (if (= n 0) 0 (+ n (sum (- n 1)))))\n(let main {} (sum 5))";
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_all_float_operators() {
    assert_eq!(check_expr("(-. 3.0 1.5)").unwrap(), Type::float());
    assert_eq!(check_expr("(*. 2.0 3.0)").unwrap(), Type::float());
    assert_eq!(check_expr("(/. 6.0 2.0)").unwrap(), Type::float());
}

#[test]
fn infer_boolean_or() {
    let ty = check_expr("(or False True)").unwrap();
    assert_eq!(ty, Type::bool());
}

#[test]
fn infer_all_int_comparisons() {
    for src in ["(< 1 2)", "(> 2 1)", "(<= 1 1)", "(>= 2 1)"] {
        let ty = check_expr(src).unwrap();
        assert_eq!(ty, Type::bool(), "failed for: {src}");
    }
}

#[test]
fn infer_int_modulo() {
    let ty = check_expr("(% 7 3)").unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_inequality_operator() {
    let ty = check_expr("(!= 1 2)").unwrap();
    assert_eq!(ty, Type::bool());
}

#[test]
fn infer_nested_sequential_let_with_deps() {
    // y depends on x from the same binding block
    let src = "(let helper {dummy} (let [x 5 y (+ x 3)] y))\n(let main {} (helper 0))";
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_let_binding_shadows_outer() {
    // Inner x shadows the outer x — wrapped in a function since local bindings
    // are not valid at the top level
    let src = "(let helper {dummy} (let [x 1] (let [x True] x)))\n(let main {} (helper 0))";
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::bool());
}

#[test]
fn infer_match_on_bool_literal() {
    let src = "(match True True ~> 1 False ~> 0)";
    let ty = check_expr(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_match_variable_pattern_binds() {
    // Variable pattern in match binds the matched value
    let src = "(match 42 n ~> (+ n 1))";
    let ty = check_expr(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_match_guard_typechecks() {
    let src = "(match 42 n if (> n 0) ~> n _ ~> 0)";
    let ty = check_expr(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn reject_match_guard_non_bool() {
    let src = "(match 42 n if n ~> n _ ~> 0)";
    let result = check_expr(src);
    assert!(result.is_err(), "expected non-bool guard to fail");
}

#[test]
fn infer_partial_application() {
    // (let add {a b} ...) gives add : Int -> Int -> Int
    // (add 1) gives Int -> Int
    let src = "(let add {a b} (+ a b))\n(let main {} (add 1))";
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::fun(Type::int(), Type::int()));
}

#[test]
fn infer_function_as_argument() {
    // Pass `not` as a higher-order argument
    let src = "(let apply {func x} (func x))\n(let main {} (apply not True))";
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::bool());
}

#[test]
fn infer_list_of_bools() {
    let ty = check_expr("[True False True]").unwrap();
    assert_eq!(ty, Type::array(Type::bool()));
}

#[test]
fn infer_list_of_floats() {
    let ty = check_expr("[1.0 2.5 3.14]").unwrap();
    assert_eq!(ty, Type::array(Type::float()));
}

#[test]
fn infer_variant_in_match_with_binding() {
    let src = r#"
            (type ['a] Option [None (Some ~ 'a)])
            (let unwrap_or {opt default}
              (match opt
                (Some x) ~> x
                None     ~> default))
            (let main {} (unwrap_or (Some 99) 0))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_record_pattern_with_partial_destructuring() {
    let src = r#"
            (type Person [(:name ~ String) (:age ~ Int)])
            (let age_of {person}
              (match person
                (Person :age age) ~> age))
            (let main {} (age_of (Person "Ada" 37)))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_nested_record_pattern() {
    let src = r#"
            (type Address [(:city ~ String)])
            (type Person [(:name ~ String) (:address ~ Address)])
            (let city_of {person}
              (match person
                (Person :address (Address :city city)) ~> city))
            (let main {} (city_of (Person "Ada" (Address "London"))))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::string());
}

#[test]
fn reject_unknown_field_in_record_pattern() {
    let src = r#"
            (type Person [(:name ~ String) (:age ~ Int)])
            (let age_of {person}
              (match person
                (Person :height h) ~> h))
        "#;
    let err = check(src).expect_err("expected unknown field error");
    match err {
        TypeError::UnknownField { field, .. } => assert_eq!(field, "height"),
        other => panic!("expected UnknownField, got {other:?}"),
    }
}

#[test]
fn reject_non_exhaustive_variant_match() {
    let src = r#"
            (type LotsOVariants [One Two (Three ~ Int) Four Five (Six ~ String)])
            (let main {}
              (let [x One]
                (match x
                  One ~> ()
                  Two ~> ())))
        "#;
    let err = check(src).expect_err("expected non-exhaustive match");
    match err {
        TypeError::NonExhaustiveMatch { missing, .. } => {
            assert_eq!(missing, vec!["Three", "Four", "Five", "Six"]);
        }
        other => panic!("expected NonExhaustiveMatch, got {other:?}"),
    }
}

#[test]
fn accept_exhaustive_variant_match_with_or_pattern() {
    let src = r#"
            (type TrafficLight [Red Yellow Green])
            (let to_int {light}
              (match light
                Red | Yellow ~> 0
                Green ~> 1))
            (let main {} (to_int Red))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn reject_non_exhaustive_list_match_missing_empty() {
    let src = r#"
            (let classify {xs}
              (match xs
                [h | t] ~> 1))
            (let main {} (classify [1]))
        "#;
    let err = check(src).expect_err("expected non-exhaustive match");
    match err {
        TypeError::NonExhaustiveMatch { missing, .. } => {
            assert_eq!(missing, vec!["[]"]);
        }
        other => panic!("expected NonExhaustiveMatch, got {other:?}"),
    }
}

#[test]
fn reject_non_exhaustive_list_match_missing_cons() {
    let src = r#"
            (let classify {xs}
              (match xs
                [] ~> 0))
            (let main {} (classify []))
        "#;
    let err = check(src).expect_err("expected non-exhaustive match");
    match err {
        TypeError::NonExhaustiveMatch { missing, .. } => {
            assert_eq!(missing, vec!["[head | tail]"]);
        }
        other => panic!("expected NonExhaustiveMatch, got {other:?}"),
    }
}

#[test]
fn accept_exhaustive_list_match() {
    let src = r#"
            (let classify {xs}
              (match xs
                [] ~> 0
                [h | t] ~> 1))
            (let main {} (classify []))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn reject_non_exhaustive_bool_match() {
    let err = check_expr("(match True True ~> 1)").expect_err("expected non-exhaustive match");
    match err {
        TypeError::NonExhaustiveMatch { missing, .. } => {
            assert_eq!(missing, vec!["False"]);
        }
        other => panic!("expected NonExhaustiveMatch, got {other:?}"),
    }
}

#[test]
fn accept_exhaustive_bool_match_with_or_pattern() {
    let ty = check_expr("(match True True | False ~> 1)").unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn reject_non_exhaustive_int_match_requires_catch_all() {
    let err = check_expr("(match 7 7 ~> 1)").expect_err("expected non-exhaustive match");
    match err {
        TypeError::NonExhaustiveMatch { missing, .. } => {
            assert_eq!(missing, vec!["_"]);
        }
        other => panic!("expected NonExhaustiveMatch, got {other:?}"),
    }
}

#[test]
fn accept_exhaustive_int_match_with_catch_all() {
    let ty = check_expr("(match 7 7 ~> 1 _ ~> 0)").unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn accept_mutual_recursion() {
    let src = r#"
            (let even {n} (if (= n 0) True (odd (- n 1))))
            (let odd  {n} (if (= n 0) False (even (- n 1))))
            (let main {} (even 4))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::bool());
}

#[test]
fn accept_forward_reference_to_later_top_level_function() {
    let src = r#"
            (let main {} (helper 10))
            (let helper {x} (+ x 1))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::fun(Type::int(), Type::int()));
}

// -------------------------------------------------------------------------
// Typecheck rejection tests — invalid programs that must fail
// -------------------------------------------------------------------------

#[test]
fn reject_apply_non_function() {
    // (1 2) — applying an integer is a type error
    let result = check_expr("(1 2)");
    assert!(result.is_err(), "expected type error, got {:?}", result);
}

#[test]
fn reject_or_with_non_bool() {
    assert!(
        check_expr("(or 1 2)").is_err(),
        "expected error: or requires Bool"
    );
    assert!(
        check_expr("(or True 1)").is_err(),
        "expected error: second arg must be Bool"
    );
}

#[test]
fn reject_and_with_non_bool() {
    assert!(
        check_expr("(and 1 True)").is_err(),
        "expected error: first arg must be Bool"
    );
}

#[test]
fn reject_not_with_non_bool() {
    let result = check_expr("(not 42)");
    assert!(result.is_err(), "expected error: not requires Bool");
}

#[test]
fn reject_equality_on_mismatched_types() {
    // (= 1 True) — polymorphic equality requires both sides same type
    let result = check_expr("(= 1 True)");
    assert!(result.is_err(), "expected type error for (= 1 True)");
}

#[test]
fn reject_float_op_with_int() {
    // (+. 1 2.5) — float arithmetic requires both operands to be Float
    let result = check_expr("(+. 1 2.5)");
    assert!(
        result.is_err(),
        "expected type error: Int used with float op"
    );
}

#[test]
fn reject_int_op_with_float() {
    // (+ 1.0 2.0) — int arithmetic requires Int
    let result = check_expr("(+ 1.0 2.0)");
    assert!(
        result.is_err(),
        "expected type error: Float used with int op"
    );
}

#[test]
fn reject_int_modulo_with_float() {
    let result = check_expr("(% 7.0 2)");
    assert!(
        result.is_err(),
        "expected type error: Float used with int modulo"
    );
}

#[test]
fn reject_int_comparison_with_float() {
    // Comparison operators are Int-only
    let result = check_expr("(< 1.0 2.0)");
    assert!(result.is_err(), "expected type error: < requires Int");
}

#[test]
fn infer_all_float_comparisons() {
    for src in [
        "(<. 1.0 2.0)",
        "(>. 2.0 1.0)",
        "(<=. 1.0 1.0)",
        "(>=. 2.0 1.0)",
    ] {
        let ty = check_expr(src).unwrap();
        assert_eq!(ty, Type::bool(), "failed for: {src}");
    }
}

#[test]
fn reject_float_comparison_with_int() {
    let result = check_expr("(<. 1 2)");
    assert!(result.is_err(), "expected type error: <. requires Float");
}

#[test]
fn reject_match_arms_type_mismatch() {
    // Arms return different types: Int vs Bool
    let result = check_expr("(match True True ~> 1 False ~> False)");
    assert!(
        matches!(
            result,
            Err(TypeError::ArmMismatch {
                arm: 1,
                from_letq_desugar: false,
                ..
            })
        ),
        "expected ArmMismatch on arm 2, got {result:?}"
    );
}

#[test]
fn reject_letq_continuation_that_does_not_return_result() {
    let src = r#"
            (type ['a 'e] Result [
                (Ok ~ 'a)
                (Error ~ 'e)])
            (let main {}
              (let? [x (Ok 1)]
                x))
        "#;
    let result = check(src);
    assert!(
        matches!(
            result,
            Err(TypeError::ArmMismatch {
                arm: 1,
                from_letq_desugar: true,
                ..
            })
        ),
        "expected let? ArmMismatch on error arm, got {result:?}"
    );
}

#[test]
fn extern_declaration_adds_to_env() {
    // extern makes the name available with the declared type
    let src = r#"
            (extern let my_print ~ (String -> Unit) io/format)
            (let main {} (my_print "hello"))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::unit());
}

#[test]
fn extern_wrong_arg_type_is_rejected() {
    let src = r#"
            (extern let my_print ~ (String -> Unit) io/format)
            (let main {} (my_print 42))
        "#;
    assert!(check(src).is_err());
}

#[test]
fn infer_string_literal_pattern() {
    let src = r#"(let greet {s} (match s "hello" ~> "hi!" _ ~> "?"))
(let main {} (greet "hello"))"#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::string());
}

#[test]
fn infer_or_pattern_match() {
    // All alternatives are Int literals; result is Bool
    let src = "(let pred {x} (match x 1 | 2 | 3 ~> True _ ~> False))\n(let main {} (pred 1))";
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::bool());
}

#[test]
fn reject_or_pattern_type_mismatch() {
    // or-pattern alternatives must agree with the target type; 1 is Int, True is Bool
    let result = check("(let pred {x} (match x 1 | True ~> 0 _ ~> 1))\n(let main {} (pred 1))");
    assert!(
        result.is_err(),
        "expected type error: Int vs Bool in or-pattern"
    );
}

#[test]
fn infer_multi_target_match() {
    // Two targets, two patterns per arm; both arms return Bool
    let src = "(let both {x y} (match x y 1 1 ~> True _ _ ~> False))\n(let main {} (both 1 1))";
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::bool());
}

#[test]
fn infer_empty_list_pattern() {
    let src = "(let classify {lst} (match lst [] ~> 0 [h | _] ~> 1))\n(let main {} (classify []))";
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_cons_pattern_binds_head() {
    // h is bound to the head element — must be Int since the list is Int list
    let src = "(let head_or_zero {lst} (match lst [] ~> 0 [h | _] ~> h))\n(let main {} (head_or_zero [1 2 3]))";
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_cons_pattern_binds_tail() {
    // t is bound to the tail — must be List Int
    let src = "(let tail_or_self {lst} (match lst [] ~> lst [_ | t] ~> t))\n(let main {} (tail_or_self [1 2]))";
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::array(Type::int()));
}

#[test]
fn infer_recursive_list_function() {
    // count elements using [] and [h | t] — classic recursive list function
    let src = r#"
            (let count {lst acc}
              (match lst
                [] ~> acc
                [_ | t] ~> (count t (+ acc 1))))
            (let main {} (count [1 2 3] 0))
        "#;
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_list_pattern_wildcard_tail() {
    // `_` is valid as the tail pattern
    let src = "(let first {lst} (match lst [] ~> 0 [h | _] ~> h))\n(let main {} (first [5]))";
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_singleton_list_pattern_sugar() {
    // `[x]` is sugar for `[x | []]`
    let src = "(let only_singleton {lst} (match lst [x] ~> x _ ~> 0))\n(let main {} (only_singleton [5]))";
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::int());
}

#[test]
fn infer_multi_head_cons_pattern_sugar() {
    // `[a b | rest]` is sugar for `[a | [b | rest]]`
    let src = "(let drop_two {lst} (match lst [a b | rest] ~> rest _ ~> []))\n(let main {} (drop_two [1 2 3]))";
    let ty = check(src).unwrap();
    assert_eq!(ty, Type::array(Type::int()));
}

#[test]
fn reject_cons_pattern_wrong_element_type() {
    // list is List Int but arm body uses h as Bool — should fail
    let src = r#"
            (let only_bool_list {lst}
              (match lst
                [] ~> False
                [h | _] ~> (= h True)))
            (let main {} (only_bool_list [1 2]))
        "#;
    assert!(
        check(src).is_err(),
        "expected type error: Bool vs Int in cons pattern"
    );
}

#[test]
fn reject_let_binding_type_error_in_sequence() {
    // x is Bool, but (+ x 1) requires Int
    let result = check("(let helper {dummy} (let [x True y (+ x 1)] y))");
    assert!(
        result.is_err(),
        "expected type error: Bool used in int addition"
    );
}

#[test]
fn reject_if_condition_not_bool() {
    let result = check_expr(r#"(if "hello" 1 2)"#);
    assert!(result.is_err(), "expected type error: String is not Bool");
}

#[test]
fn reject_unbound_in_let_body() {
    // The function `helper` uses `unknown` which is not in scope
    let result = check("(let helper {x} unknown)");
    assert!(
        matches!(result, Err(TypeError::UnboundVariable(_, _))),
        "expected UnboundVariable, got {:?}",
        result
    );
}

#[test]
fn reject_field_access_on_non_record() {
    // Accessing :x on an Int literal must fail
    let src = r#"
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let test {} (:x 42))
        "#;
    let result = check(src);
    assert!(result.is_err(), "expected type error: :x applied to Int");
}

#[test]
fn reject_unknown_field_on_record() {
    // :z is not a field of Point — fails during the function's type-check
    let src = r#"
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let get_z {p} (:z p))
        "#;
    let result = check(src);
    assert!(
        matches!(result, Err(TypeError::UnknownField { .. })),
        "expected UnknownField for unknown field :z"
    );
}

#[test]
fn reject_wrong_constructor_arg_type() {
    // (+ s True) — s is Int (from Some 1), True is Bool — type error
    let src = r#"
            (type ['a] Option [None (Some ~ 'a)])
            (let bad_match {x} (match x (Some s) ~> (+ s True) None ~> 0))
        "#;
    let result = check(src);
    assert!(result.is_err(), "expected type error: Bool used with +");
}
