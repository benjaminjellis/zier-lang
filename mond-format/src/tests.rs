use crate::format;

fn fmt(src: &str) -> String {
    format(src, 80)
}

// ── Atoms and literals ────────────────────────────────────────────────────

#[test]
fn atom_passthrough() {
    assert_eq!(fmt("42"), "42\n");
}

// ── let func ─────────────────────────────────────────────────────────────

#[test]
fn let_func_inline() {
    assert_eq!(
        fmt("(let add {a b} (+ a b))"),
        "(let add {a b}\n  (+ a b))\n"
    );
}

#[test]
fn let_func_pub() {
    assert_eq!(
        fmt("(pub let add {a b} (+ a b))"),
        "(pub let add {a b}\n  (+ a b))\n"
    );
}

#[test]
fn let_func_zero_args() {
    assert_eq!(fmt("(let main {} 42)"), "(let main {}\n  42)\n");
}

#[test]
fn let_func_breaks_long_body() {
    // Body is 64 chars; with prefix "(let run {} " (12 chars), total still forces a wrap.
    // Make it definitely not fit:
    let src =
        "(let run {} (some_really_long_function_name_that_makes_the_line_too_long arg1 arg2))";
    let out = fmt(src);
    assert!(out.contains("\n  "), "expected body on new line:\n{out}");
}

#[test]
fn let_func_multi_body() {
    // Multiple body expressions always break
    let src = "(let main {} expr1 expr2)";
    let out = fmt(src);
    assert!(
        out.contains('\n'),
        "expected line break for multi-body:\n{out}"
    );
}

// ── let local (bindings) ─────────────────────────────────────────────────

#[test]
fn let_local_inline() {
    assert_eq!(
        fmt("(let [x 1 y 2] (+ x y))"),
        "(let [x 1 y 2]\n  (+ x y))\n"
    );
}

#[test]
fn let_local_breaks_long() {
    let src = "(let [very_long_name_one some_value very_long_name_two another_value] body)";
    let out = fmt(src);
    assert!(out.contains('\n'));
}

#[test]
fn let_local_long_bindings_break_per_pair() {
    let src = "(let [me (self) pid (spawn (f {} -> (worker me))) x 10 y 20 z 90 g 500] (send pid \"ping\"))";
    let out = fmt(src);
    assert!(
        out.contains("[me"),
        "expected bindings vector output:\n{out}"
    );
    assert!(
        out.contains("\n      pid (spawn"),
        "expected `pid` binding on its own line:\n{out}"
    );
    assert!(
        out.contains("\n      x"),
        "expected `x` binding on its own line:\n{out}"
    );
}

// ── if ────────────────────────────────────────────────────────────────────

#[test]
fn if_inline() {
    assert_eq!(fmt("(if True 1 0)"), "(if True 1 0)\n");
}

#[test]
fn if_breaks_long() {
    let src =
        "(if (some_long_condition x y) (do_the_thing_when_true a b) (do_the_thing_when_false a b))";
    let out = fmt(src);
    assert!(out.contains('\n'));
}

// ── match ─────────────────────────────────────────────────────────────────

#[test]
fn match_inside_function() {
    let src = r#"(let check_two {x} (match x 10 or 11 or 12 ~> (println "expected") _ ~> (println "not expected")))"#;
    let out = fmt(src);
    let expected = "(let check_two {x}\n  (match x\n    10 or 11 or 12 ~> (println \"expected\")\n    _ ~> (println \"not expected\")))\n";
    assert_eq!(out, expected);
}

#[test]
fn match_arms_on_lines() {
    let src = "(match n 0 ~> 1 _ ~> (+ n 1))";
    let out = fmt(src);
    assert_eq!(out, "(match n\n  0 ~> 1\n  _ ~> (+ n 1))\n");
}

#[test]
fn match_constructor_arm() {
    let src = "(match x None ~> 0 (Some v) ~> v)";
    let out = fmt(src);
    assert!(out.contains("~>"));
    assert!(out.contains("None ~> 0"));
    assert!(out.contains("(Some v) ~> v"));
}

#[test]
fn nested_match_arm_body_breaks_after_arrow() {
    let src = "(match (self) me ~> (match (spawn (f {} -> (worker me))) pid ~> (do (send pid \"ping\") (match (receive_timeout 1000) (Ok x) ~> (do (io/println \"main got reply~n\") (io/debug x)) (Error _) ~> (io/println \"timed out~n\")))))";
    let out = fmt(src);
    assert!(
        out.contains("me ~>\n"),
        "expected outer arm body to break onto next line:\n{out}"
    );
    assert!(
        out.contains("pid ~>\n"),
        "expected inner arm body to break onto next line:\n{out}"
    );
}

// ── type ──────────────────────────────────────────────────────────────────

#[test]
fn type_variant_inline() {
    let src = "(type ['a] Option [None (Some ~ 'a)])";
    let out = fmt(src);
    assert_eq!(out, "(type ['a] Option [\n  None\n  (Some ~ 'a)])\n");
}

#[test]
fn type_record_inline() {
    let src = "(type Point [(:x ~ Int) (:y ~ Int)])";
    let out = fmt(src);
    assert_eq!(out, "(type Point [\n  (:x ~ Int)\n  (:y ~ Int)])\n");
}

#[test]
fn type_pub() {
    let src = "(pub type Foo [A B])";
    let out = fmt(src);
    assert_eq!(out, "(pub type Foo [\n  A\n  B])\n");
}

#[test]
fn type_variants_break_onto_new_lines() {
    let src = "(type LotsOVariants [ One Two (Three ~ Int) Four Five (Six ~ String) ])";
    let out = fmt(src);
    let expected = "(type LotsOVariants [\n  One\n  Two\n  (Three ~ Int)\n  Four\n  Five\n  (Six ~ String)])\n";
    assert_eq!(out, expected);
}

// ── use (always inline, consecutive uses grouped) ─────────────────────────

#[test]
fn use_inline() {
    assert_eq!(fmt("(use std/io)"), "(use std/io)\n");
}

#[test]
fn consecutive_uses_no_blank_line() {
    let src = "(use std/io)\n(use std/result)\n(use std/string)";
    let out = fmt(src);
    assert_eq!(out, "(use std/io)\n(use std/result)\n(use std/string)\n");
}

#[test]
fn use_then_let_gets_blank_line() {
    let src = "(use std/io)\n(let ident {x} x)";
    let out = fmt(src);
    assert_eq!(out, "(use std/io)\n\n(let ident {x}\n  x)\n");
}

// ── generic call ──────────────────────────────────────────────────────────

#[test]
fn call_inline() {
    assert_eq!(fmt("(foo a b c)"), "(foo a b c)\n");
}

#[test]
fn call_breaks_when_long() {
    let src =
        "(very_long_function_name_here argument_one argument_two argument_three argument_four)";
    let out = fmt(src);
    assert!(out.contains('\n'));
}

#[test]
fn pipe_breaks_one_step_per_line() {
    let src = "(let [x (|> 10 (add_two 1) (add_two 12) (add_two 20) (add_two 2) (add_two 90))] (io/debug x))";
    let out = fmt(src);
    assert!(
        out.contains("(|> 10\n"),
        "expected pipe to break after seed value:\n{out}"
    );
    assert!(
        out.contains("\n            (add_two 1)"),
        "expected first pipe step on its own line:\n{out}"
    );
    assert!(
        out.contains("\n            (add_two 90))"),
        "expected last pipe step on its own line:\n{out}"
    );
}

// ── multiple top-level forms ──────────────────────────────────────────────

#[test]
fn multiple_top_level_separated_by_blank_line() {
    let src = "(let ident {x} x)(let g {x} x)";
    let out = fmt(src);
    assert!(
        out.contains("\n\n"),
        "expected blank line between forms:\n{out}"
    );
}

// ── idempotency ───────────────────────────────────────────────────────────

#[test]
fn idempotent_simple() {
    let src = "(let add {a b} (+ a b))";
    let once = fmt(src);
    let twice = fmt(once.trim());
    assert_eq!(once, twice);
}

#[test]
fn idempotent_if() {
    let src = "(let ident {x} (if True x 0))";
    let once = fmt(src);
    let twice = fmt(once.trim());
    assert_eq!(once, twice);
}

#[test]
fn idempotent_match() {
    let src = "(let ident {n} (match n 0 ~> 1 _ ~> (+ n 1)))";
    let once = fmt(src);
    let twice = fmt(once.trim());
    assert_eq!(
        once, twice,
        "match is not idempotent:\nFirst:\n{once}\nSecond:\n{twice}"
    );
}

#[test]
fn idempotent_type() {
    let src = "(type ['a] Option [None (Some ~ 'a)])";
    let once = fmt(src);
    let twice = fmt(once.trim());
    assert_eq!(
        once, twice,
        "type is not idempotent:\nFirst:\n{once}\nSecond:\n{twice}"
    );
}

// ── comments ─────────────────────────────────────────────────────────────

#[test]
fn leading_comment_before_form() {
    let src = ";; docs for ident\n(let ident {x} x)";
    assert_eq!(fmt(src), ";; docs for ident\n(let ident {x}\n  x)\n");
}

#[test]
fn comment_block_with_blank_line_preserved() {
    let src = ";; header\n\n;; docs for ident\n(let ident {x} x)";
    assert_eq!(
        fmt(src),
        ";; header\n\n;; docs for ident\n(let ident {x}\n  x)\n"
    );
}

#[test]
fn comment_between_forms() {
    let src = "(let ident {x} x)\n\n;; docs for g\n(let g {x} x)";
    let out = fmt(src);
    assert!(
        out.contains(";; docs for g\n(let g {x}\n  x)"),
        "comment should precede g:\n{out}"
    );
}

#[test]
fn trailing_comment() {
    let src = "(let ident {x} x)\n\n;; end of file";
    let out = fmt(src);
    assert!(
        out.contains(";; end of file"),
        "trailing comment missing:\n{out}"
    );
}

#[test]
fn idempotent_with_comments() {
    let src = ";; header\n\n;; docs\n(let ident {x} x)\n\n;; more docs\n(let g {x} x)";
    let once = fmt(src);
    let twice = fmt(once.trim());
    assert_eq!(
        once, twice,
        "not idempotent with comments:\n{once}\n---\n{twice}"
    );
}

// ── real program ─────────────────────────────────────────────────────────

#[test]
fn formats_real_program() {
    let src = r#"
(use std/io)
(use std/result)

(let safe_div {a b}
  (if (= b 0)
    (Error "division by zero")
    (Ok (/ a b))))

(type SimpleRecord [
  (:a ~ Int)
  (:b ~ Int)
])

(let main {}
  (let [a (SimpleRecord :a 10 :b 10)] (io/debug a))
  (io/println "hello world~n"))
"#;
    let out = format(src.trim(), 80);
    let out2 = format(out.trim(), 80);
    assert_eq!(
        out, out2,
        "formatter is not idempotent:\nFirst pass:\n{out}\nSecond pass:\n{out2}"
    );
}

#[test]
fn extern_type_sig_flat() {
    let src = "(pub extern let put ~ ('k -> 'v -> Map 'k 'v -> Map 'k 'v) maps/put)";
    assert_eq!(format(src, 100), format!("{src}\n"));
}

// ── do ────────────────────────────────────────────────────────────────────

#[test]
fn do_single_inline() {
    assert_eq!(fmt("(do (call x))"), "(do (call x))\n");
}

#[test]
fn do_multi_breaks() {
    let src = "(do (io/debug h) (iterate t))";
    let out = fmt(src); // always breaks regardless of width
    assert_eq!(out, "(do (io/debug h)\n    (iterate t))\n");
}

#[test]
fn do_idempotent() {
    let src = "(let debug_then_print {x} (do (io/debug x) (io/println \"done\")))";
    let out = format(src, 80);
    let out2 = format(&out, 80);
    assert_eq!(out, out2, "formatter is not idempotent on do");
}

#[test]
fn do_in_match_arm() {
    let src = r#"(let iterate {list} (match list [] ~> (io/println "empty") [h | t] ~> (do (io/debug h) (iterate t))))"#;
    let out = format(src, 80);
    let out2 = format(&out, 80);
    assert_eq!(out, out2, "formatter is not idempotent");
    assert!(out.contains("(do ("), "do should keep first expr inline");
}

#[test]
fn extern_type_sig_idempotent() {
    let src = "(pub extern type ['k 'v] Map maps/map)\n(pub extern let new ~ (Unit -> (Map 'k 'v)) maps/new)";
    let out = format(src, 100);
    assert!(!out.contains("'k\n"), "type sig is breaking badly:\n{out}");
    let out2 = format(&out, 100);
    assert_eq!(out, out2, "formatter is not idempotent");
}
