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
    assert_eq!(fmt("(let add {a b} (+ a b))"), "(let add {a b} (+ a b))\n");
}

#[test]
fn let_func_pub() {
    assert_eq!(
        fmt("(pub let add {a b} (+ a b))"),
        "(pub let add {a b} (+ a b))\n"
    );
}

#[test]
fn let_func_zero_args() {
    assert_eq!(fmt("(let main {} 42)"), "(let main {} 42)\n");
}

#[test]
fn let_func_breaks_long_body() {
    // Body is 64 chars; with prefix "(let f {} " (10 chars), total = 74, fits.
    // Make it definitely not fit:
    let src = "(let f {} (some_really_long_function_name_that_makes_the_line_too_long arg1 arg2))";
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
    assert_eq!(fmt("(let [x 1 y 2] (+ x y))"), "(let [x 1 y 2] (+ x y))\n");
}

#[test]
fn let_local_breaks_long() {
    let src = "(let [very_long_name_one some_value very_long_name_two another_value] body)";
    let out = fmt(src);
    assert!(out.contains('\n'));
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

// ── type ──────────────────────────────────────────────────────────────────

#[test]
fn type_variant_inline() {
    // Short enough to fit on one line
    let src = "(type ['a] Option (None (Some ~ 'a)))";
    let out = fmt(src);
    assert_eq!(out, "(type ['a] Option ( None (Some ~ 'a) ))\n");
}

#[test]
fn type_record_inline() {
    let src = "(type Point ((:x ~ Int) (:y ~ Int)))";
    let out = fmt(src);
    assert_eq!(out, "(type Point ( (:x ~ Int) (:y ~ Int) ))\n");
}

#[test]
fn type_pub() {
    let src = "(pub type Foo (A B))";
    let out = fmt(src);
    assert_eq!(out, "(pub type Foo ( A B ))\n");
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
    let src = "(use std/io)\n(let f {x} x)";
    let out = fmt(src);
    assert_eq!(out, "(use std/io)\n\n(let f {x} x)\n");
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

// ── multiple top-level forms ──────────────────────────────────────────────

#[test]
fn multiple_top_level_separated_by_blank_line() {
    let src = "(let f {x} x)(let g {x} x)";
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
    let src = "(let f {x} (if True x 0))";
    let once = fmt(src);
    let twice = fmt(once.trim());
    assert_eq!(once, twice);
}

#[test]
fn idempotent_match() {
    let src = "(let f {n} (match n 0 ~> 1 _ ~> (+ n 1)))";
    let once = fmt(src);
    let twice = fmt(once.trim());
    assert_eq!(
        once, twice,
        "match is not idempotent:\nFirst:\n{once}\nSecond:\n{twice}"
    );
}

#[test]
fn idempotent_type() {
    let src = "(type ['a] Option (None (Some ~ 'a)))";
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
    let src = ";; docs for f\n(let f {x} x)";
    assert_eq!(fmt(src), ";; docs for f\n(let f {x} x)\n");
}

#[test]
fn comment_block_with_blank_line_preserved() {
    let src = ";; header\n\n;; docs for f\n(let f {x} x)";
    assert_eq!(fmt(src), ";; header\n\n;; docs for f\n(let f {x} x)\n");
}

#[test]
fn comment_between_forms() {
    let src = "(let f {x} x)\n\n;; docs for g\n(let g {x} x)";
    let out = fmt(src);
    assert!(
        out.contains(";; docs for g\n(let g {x} x)"),
        "comment should precede g:\n{out}"
    );
}

#[test]
fn trailing_comment() {
    let src = "(let f {x} x)\n\n;; end of file";
    let out = fmt(src);
    assert!(
        out.contains(";; end of file"),
        "trailing comment missing:\n{out}"
    );
}

#[test]
fn idempotent_with_comments() {
    let src = ";; header\n\n;; docs\n(let f {x} x)\n\n;; more docs\n(let g {x} x)";
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

(type SimpleRecord (
  (:a ~ Int)
  (:b ~ Int)
))

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
