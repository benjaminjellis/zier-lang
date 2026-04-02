use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use crate::{ast, ir, pipeline::CompileTarget, typecheck};

// ─── Context ────────────────────────────────────────────────────────────────

struct Ctx {
    /// Names of all top-level functions (used to distinguish fn-refs from local vars)
    fn_names: HashSet<String>,
    /// Top-level function name → number of Mond args (for multi-arg TCO)
    fn_arities: HashMap<String, usize>,
    /// Local extern declarations marked nullary `(Unit -> T)` lowered to `/0`.
    nullary_externs: HashSet<String>,
    /// Constructor name → arity (0 = nullary atom, 1+ = tuple)
    constructors: HashMap<String, usize>,
    /// Imported function name → Erlang module (from `use` declarations)
    imports: HashMap<String, String>,
    /// User-facing module name → Erlang module name (e.g. "time" → "d_time_time")
    module_aliases: HashMap<String, String>,
    /// Record/field pair → 1-based element index (tag is at 1, fields start at 2)
    field_indices: HashMap<(String, String), usize>,
    /// Record type name -> field names in declaration order
    record_layouts: HashMap<String, Vec<String>>,
    /// Expression span key (start, end) -> inferred record type name
    record_expr_types: HashMap<(usize, usize), String>,
    /// Expression span key (start, end) -> inferred expression type
    expr_types: HashMap<(usize, usize), Arc<typecheck::Type>>,
    /// Local and imported type declarations keyed by type name
    type_decls: HashMap<String, ast::TypeDecl>,
    /// Compile mode controls whether debug lowers to rich or fallback output.
    compile_target: CompileTarget,
}

pub struct LowerModuleInput {
    pub compile_target: CompileTarget,
    pub imports: HashMap<String, String>,
    pub module_aliases: HashMap<String, String>,
    pub imported_type_decls: Vec<ast::TypeDecl>,
    pub imported_constructors: HashMap<String, usize>,
    pub imported_field_indices: HashMap<(String, String), usize>,
    pub imported_record_layouts: HashMap<String, Vec<String>>,
    pub inferred_expr_types: HashMap<(usize, usize), Arc<typecheck::Type>>,
    pub inferred_record_expr_types: HashMap<(usize, usize), String>,
}

const DEBUG_HELPER_NAME: &str = "mond$debug";

// ─── Public entry point ─────────────────────────────────────────────────────

pub fn lower_module(name: &str, decls: &[ast::Declaration], input: LowerModuleInput) -> ir::Module {
    let LowerModuleInput {
        compile_target,
        imports,
        module_aliases,
        imported_type_decls,
        imported_constructors,
        imported_field_indices,
        imported_record_layouts,
        inferred_expr_types,
        inferred_record_expr_types,
    } = input;

    // Pass 1: collect function names, arities, constructor arities, and record field indices
    let mut fn_names = HashSet::new();
    let mut fn_arities = HashMap::new();
    let mut nullary_externs = HashSet::new();
    let mut constructors = imported_constructors;
    let mut field_indices = imported_field_indices;
    let mut record_layouts = imported_record_layouts;
    let mut type_decls = HashMap::new();
    for type_decl in imported_type_decls {
        let qualified_name = type_decl_name(&type_decl).to_string();
        type_decls.insert(qualified_name.clone(), type_decl.clone());
        if let Some((_, unqualified_name)) = qualified_name.rsplit_once('/') {
            type_decls
                .entry(unqualified_name.to_string())
                .or_insert_with(|| {
                    clone_type_decl_with_name(&type_decl, unqualified_name.to_string())
                });
        }
    }

    for decl in decls {
        match decl {
            ast::Declaration::Expression(ast::Expr::LetFunc { name, args, .. }) => {
                fn_names.insert(name.clone());
                fn_arities.insert(name.clone(), args.len());
            }
            ast::Declaration::ExternLet {
                name, is_nullary, ..
            } => {
                fn_names.insert(name.clone());
                if *is_nullary {
                    nullary_externs.insert(name.clone());
                }
            }
            ast::Declaration::Type(ast::TypeDecl::Variant {
                constructors: ctors,
                ..
            }) => {
                for (ctor_name, payloads) in ctors {
                    constructors.insert(ctor_name.clone(), payloads.len());
                }
            }
            ast::Declaration::Type(ast::TypeDecl::Record { name, fields, .. }) => {
                // Tag is at element(1), fields start at element(2)
                for (i, (field_name, _)) in fields.iter().enumerate() {
                    field_indices.insert((name.clone(), field_name.clone()), i + 2);
                }
                // Records are constructors too: {record_tag, field1, field2, ...}
                constructors.insert(name.clone(), fields.len());
                record_layouts.insert(
                    name.clone(),
                    fields
                        .iter()
                        .map(|(field_name, _)| field_name.clone())
                        .collect(),
                );
            }
            _ => {}
        }

        if let ast::Declaration::Type(type_decl) = decl {
            type_decls.insert(type_decl_name(type_decl).to_string(), type_decl.clone());
        }
    }

    let ctx = Ctx {
        fn_names,
        fn_arities,
        nullary_externs,
        constructors,
        imports,
        module_aliases,
        field_indices,
        record_layouts,
        record_expr_types: inferred_record_expr_types,
        expr_types: inferred_expr_types,
        type_decls,
        compile_target,
    };

    // Pass 2: lower declarations to IR functions
    let mut functions = Vec::new();
    let mut test_idx: usize = 0;

    for decl in decls {
        match decl {
            ast::Declaration::Expression(ast::Expr::LetFunc {
                name,
                args,
                value,
                is_pub,
                ..
            }) => {
                if args.len() >= 2 {
                    // Curried entry: f(A) -> fun(B) -> ... f(A, B, ...) end ... end.
                    let all_vars: Vec<ir::Expr> =
                        args.iter().map(|a| ir::Expr::Var(var_name(a))).collect();
                    let direct = ir::Expr::LocalCallMulti(name.clone(), all_vars);
                    let curried_body = args[1..].iter().rev().fold(direct, |acc, arg| {
                        ir::Expr::Fun(var_name(arg), Box::new(acc))
                    });
                    functions.push(ir::Function {
                        name: name.clone(),
                        params: vec![var_name(&args[0])],
                        body: curried_body,
                        is_pub: *is_pub,
                    });
                    // Multi-arg impl: f(A, B, ...) -> body.  Self-recursive calls target this.
                    functions.push(ir::Function {
                        name: name.clone(),
                        params: args.iter().map(|a| var_name(a)).collect(),
                        body: lower_function_body(args, value, &ctx),
                        is_pub: false,
                    });
                } else {
                    let mut f = lower_letfunc(name, args, value, &ctx);
                    f.is_pub = *is_pub;
                    functions.push(f);
                    if args.is_empty() {
                        functions.push(ir::Function {
                            name: name.clone(),
                            params: vec![],
                            body: ir::Expr::LocalCall(
                                name.clone(),
                                Box::new(ir::Expr::Atom("unit".into())),
                            ),
                            is_pub: false,
                        });
                    }
                }
            }
            ast::Declaration::ExternLet {
                name,
                is_pub,
                is_nullary,
                ty,
                erlang_target,
                ..
            } => {
                let mut f = lower_extern_let(name, *is_nullary, ty, erlang_target);
                f.is_pub = *is_pub;
                functions.push(f);
            }
            ast::Declaration::Test { body, .. } => {
                functions.push(ir::Function {
                    name: format!("mond_test_{test_idx}"),
                    params: vec!["_Unit".to_string()],
                    body: lower_expr(body, &ctx),
                    is_pub: true,
                });
                test_idx += 1;
            }
            _ => {} // Type decls, Use, ExternType produce no Erlang functions
        }
    }

    let support_sources = if module_uses_debug(decls) {
        vec![debug_support_source(compile_target)]
    } else {
        Vec::new()
    };

    ir::Module {
        name: name.to_string(),
        functions,
        support_sources,
    }
}

// ─── Function lowering ──────────────────────────────────────────────────────

// Only called for 0-arg and 1-arg functions (N >= 2 handled inline in lower_module).
fn lower_letfunc(name: &str, args: &[String], body: &ast::Expr, ctx: &Ctx) -> ir::Function {
    let body_ir = lower_function_body(args, body, ctx);
    let param = if args.is_empty() {
        "_Unit".to_string()
    } else {
        var_name(&args[0])
    };
    ir::Function {
        name: name.to_string(),
        params: vec![param],
        body: body_ir,
        is_pub: false, // caller sets this
    }
}

fn lower_extern_let(
    name: &str,
    is_nullary: bool,
    ty: &ast::TypeSig,
    erlang_target: &(String, String),
) -> ir::Function {
    let (module, function) = erlang_target;
    let arity = if is_nullary { 0 } else { type_sig_arity(ty) };
    let params: Vec<String> = (0..arity).map(|i| format!("Arg{i}")).collect();

    let remote_args: Vec<ir::Expr> = params.iter().map(|p| ir::Expr::Var(p.clone())).collect();
    let call = ir::Expr::RemoteCall(module.clone(), function.clone(), remote_args);

    // Wrap the remote call in curried funs for arity > 1
    let body = if params.len() <= 1 {
        call
    } else {
        params[1..]
            .iter()
            .rev()
            .fold(call, |acc, p| ir::Expr::Fun(p.clone(), Box::new(acc)))
    };

    let erlang_params = params.into_iter().take(1).collect();

    ir::Function {
        name: name.to_string(),
        params: erlang_params,
        body,
        is_pub: false, // caller sets this
    }
}

fn span_key(span: &std::ops::Range<usize>) -> (usize, usize) {
    (span.start, span.end)
}

fn type_decl_name(type_decl: &ast::TypeDecl) -> &str {
    match type_decl {
        ast::TypeDecl::Record { name, .. } => name,
        ast::TypeDecl::Variant { name, .. } => name,
    }
}

fn clone_type_decl_with_name(type_decl: &ast::TypeDecl, name: String) -> ast::TypeDecl {
    match type_decl {
        ast::TypeDecl::Record {
            is_pub,
            params,
            fields,
            span,
            ..
        } => ast::TypeDecl::Record {
            is_pub: *is_pub,
            name,
            params: params.clone(),
            fields: fields.clone(),
            span: span.clone(),
        },
        ast::TypeDecl::Variant {
            is_pub,
            params,
            constructors,
            span,
            ..
        } => ast::TypeDecl::Variant {
            is_pub: *is_pub,
            name,
            params: params.clone(),
            constructors: constructors.clone(),
            span: span.clone(),
        },
    }
}

fn record_name_for_expr<'a>(expr: &'a ast::Expr, ctx: &'a Ctx) -> Option<&'a str> {
    if let Some(name) = ctx.record_expr_types.get(&span_key(&expr.span())) {
        return Some(name.as_str());
    }
    match expr {
        ast::Expr::RecordConstruct { name, .. } => Some(name.as_str()),
        ast::Expr::RecordUpdate { record, .. } => record_name_for_expr(record, ctx),
        _ => None,
    }
}

fn field_index_for_record(ctx: &Ctx, record_name: &str, field_name: &str) -> Option<usize> {
    if let Some(index) = ctx
        .field_indices
        .get(&(record_name.to_string(), field_name.to_string()))
    {
        return Some(*index);
    }

    ctx.record_layouts
        .get(record_name)
        .and_then(|layout| layout.iter().position(|declared| declared == field_name))
        .map(|position| position + 2)
}

fn constructor_tag(name: &str) -> String {
    name.rsplit_once('/')
        .map_or(name, |(_, constructor)| constructor)
        .to_lowercase()
}

fn display_name(name: &str) -> String {
    name.rsplit_once('/')
        .map_or(name, |(_, display)| display)
        .to_string()
}

fn field_indices_for_label(ctx: &Ctx, field_name: &str) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = ctx
        .field_indices
        .iter()
        .filter_map(|((record_name, declared_field), idx)| {
            if declared_field == field_name {
                Some((record_name.clone(), *idx))
            } else {
                None
            }
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn dynamic_field_access(
    field: &str,
    record_expr: ir::Expr,
    ctx: &Ctx,
    fresh_idx: &mut usize,
) -> ir::Expr {
    let candidates = field_indices_for_label(ctx, field);
    if candidates.is_empty() {
        return ir::Expr::RemoteCall(
            "erlang".into(),
            "element".into(),
            vec![ir::Expr::Int(2), record_expr],
        );
    }
    if candidates.len() == 1 {
        return ir::Expr::RemoteCall(
            "erlang".into(),
            "element".into(),
            vec![ir::Expr::Int(candidates[0].1 as i64), record_expr],
        );
    }

    let record_var = format!("Fld{}__", *fresh_idx);
    *fresh_idx += 1;
    let record_ref = ir::Expr::Var(record_var.clone());
    let tag_expr = ir::Expr::RemoteCall(
        "erlang".into(),
        "element".into(),
        vec![ir::Expr::Int(1), record_ref.clone()],
    );
    let mut arms: Vec<(ir::Pattern, ir::Expr)> = candidates
        .into_iter()
        .map(|(record_name, idx)| {
            (
                ir::Pattern::Atom(constructor_tag(&record_name)),
                ir::Expr::RemoteCall(
                    "erlang".into(),
                    "element".into(),
                    vec![ir::Expr::Int(idx as i64), record_ref.clone()],
                ),
            )
        })
        .collect();
    arms.push((
        ir::Pattern::Any,
        ir::Expr::RemoteCall(
            "erlang".into(),
            "error".into(),
            vec![ir::Expr::Tuple(vec![
                ir::Expr::Atom("badfield".into()),
                ir::Expr::Atom(field.to_lowercase()),
                record_ref.clone(),
            ])],
        ),
    ));
    ir::Expr::Let(
        record_var,
        Box::new(record_expr),
        Box::new(ir::Expr::Case(Box::new(tag_expr), arms)),
    )
}

fn dynamic_record_update(
    field: &str,
    base_record_expr: ir::Expr,
    value_expr: ir::Expr,
    ctx: &Ctx,
    fresh_idx: &mut usize,
) -> ir::Expr {
    let candidates = field_indices_for_label(ctx, field);
    if candidates.is_empty() {
        return ir::Expr::RemoteCall(
            "erlang".into(),
            "setelement".into(),
            vec![ir::Expr::Int(2), base_record_expr, value_expr],
        );
    }
    if candidates.len() == 1 {
        return ir::Expr::RemoteCall(
            "erlang".into(),
            "setelement".into(),
            vec![
                ir::Expr::Int(candidates[0].1 as i64),
                base_record_expr,
                value_expr,
            ],
        );
    }

    let record_var = format!("RUpdDyn{}__", *fresh_idx);
    *fresh_idx += 1;
    let record_ref = ir::Expr::Var(record_var.clone());
    let tag_expr = ir::Expr::RemoteCall(
        "erlang".into(),
        "element".into(),
        vec![ir::Expr::Int(1), record_ref.clone()],
    );
    let mut arms: Vec<(ir::Pattern, ir::Expr)> = candidates
        .into_iter()
        .map(|(record_name, idx)| {
            (
                ir::Pattern::Atom(constructor_tag(&record_name)),
                ir::Expr::RemoteCall(
                    "erlang".into(),
                    "setelement".into(),
                    vec![
                        ir::Expr::Int(idx as i64),
                        record_ref.clone(),
                        value_expr.clone(),
                    ],
                ),
            )
        })
        .collect();
    arms.push((
        ir::Pattern::Any,
        ir::Expr::RemoteCall(
            "erlang".into(),
            "error".into(),
            vec![ir::Expr::Tuple(vec![
                ir::Expr::Atom("badfield".into()),
                ir::Expr::Atom(field.to_lowercase()),
                record_ref.clone(),
            ])],
        ),
    ));

    ir::Expr::Let(
        record_var,
        Box::new(base_record_expr),
        Box::new(ir::Expr::Case(Box::new(tag_expr), arms)),
    )
}

fn module_uses_debug(decls: &[ast::Declaration]) -> bool {
    decls.iter().any(|decl| match decl {
        ast::Declaration::Expression(expr) => expr_uses_debug(expr),
        ast::Declaration::Test { body, .. } => expr_uses_debug(body),
        _ => false,
    })
}

fn expr_uses_debug(expr: &ast::Expr) -> bool {
    match expr {
        ast::Expr::Debug { .. } => true,
        ast::Expr::Literal(_, _) | ast::Expr::Variable(_, _) => false,
        ast::Expr::List(items, _) => items.iter().any(expr_uses_debug),
        ast::Expr::LetFunc { value, .. } => expr_uses_debug(value),
        ast::Expr::LetLocal { value, body, .. } => expr_uses_debug(value) || expr_uses_debug(body),
        ast::Expr::If {
            cond, then, els, ..
        } => expr_uses_debug(cond) || expr_uses_debug(then) || expr_uses_debug(els),
        ast::Expr::Call { func, args, .. } => {
            expr_uses_debug(func) || args.iter().any(expr_uses_debug)
        }
        ast::Expr::Match { targets, arms, .. } => {
            targets.iter().any(expr_uses_debug)
                || arms.iter().any(|arm| {
                    arm.guard.as_ref().is_some_and(expr_uses_debug) || expr_uses_debug(&arm.body)
                })
        }
        ast::Expr::FieldAccess { record, .. } => expr_uses_debug(record),
        ast::Expr::RecordConstruct { fields, .. } => {
            fields.iter().any(|(_, expr)| expr_uses_debug(expr))
        }
        ast::Expr::RecordUpdate {
            record, updates, ..
        } => expr_uses_debug(record) || updates.iter().any(|(_, expr)| expr_uses_debug(expr)),
        ast::Expr::Lambda { body, .. } => expr_uses_debug(body),
        ast::Expr::QualifiedCall { args, .. } => args.iter().any(expr_uses_debug),
    }
}

fn expr_type_for<'a>(expr: &'a ast::Expr, ctx: &'a Ctx) -> Option<&'a typecheck::Type> {
    ctx.expr_types
        .get(&span_key(&expr.span()))
        .map(|ty| ty.as_ref())
}

fn debug_schema_for_expr(expr: &ast::Expr, ctx: &Ctx) -> ir::Expr {
    let mut seen = HashSet::new();
    expr_type_for(expr, ctx)
        .map(|ty| lower_debug_schema(ty, ctx, &mut seen))
        .unwrap_or_else(|| ir::Expr::Atom("opaque".into()))
}

fn lower_debug_schema(ty: &typecheck::Type, ctx: &Ctx, seen: &mut HashSet<String>) -> ir::Expr {
    match ty {
        typecheck::Type::Fun(_, _) | typecheck::Type::Var(_) => ir::Expr::Atom("opaque".into()),
        typecheck::Type::Con(name, args) if name == "Int" => ir::Expr::Atom("int".into()),
        typecheck::Type::Con(name, args) if name == "Float" => ir::Expr::Atom("float".into()),
        typecheck::Type::Con(name, args) if name == "Bool" => ir::Expr::Atom("bool".into()),
        typecheck::Type::Con(name, args) if name == "String" => ir::Expr::Atom("string".into()),
        typecheck::Type::Con(name, args) if name == "Unit" => ir::Expr::Atom("unit".into()),
        typecheck::Type::Con(name, args) if name == "List" && args.len() == 1 => {
            ir::Expr::Tuple(vec![
                ir::Expr::Atom("list".into()),
                lower_debug_schema(args[0].as_ref(), ctx, seen),
            ])
        }
        typecheck::Type::Con(name, args) => {
            let key = typecheck::type_display(ty);
            if !seen.insert(key.clone()) {
                return ir::Expr::Atom("opaque".into());
            }
            let schema = ctx
                .type_decls
                .get(name.as_str())
                .map(|type_decl| lower_named_type_schema(type_decl, args, ctx, seen))
                .unwrap_or_else(|| ir::Expr::Atom("opaque".into()));
            seen.remove(&key);
            schema
        }
    }
}

fn lower_named_type_schema(
    type_decl: &ast::TypeDecl,
    args: &[Arc<typecheck::Type>],
    ctx: &Ctx,
    seen: &mut HashSet<String>,
) -> ir::Expr {
    let (params, bindings) = match type_decl {
        ast::TypeDecl::Record { params, .. } | ast::TypeDecl::Variant { params, .. } => {
            let bindings = params
                .iter()
                .cloned()
                .zip(args.iter().cloned())
                .collect::<HashMap<_, _>>();
            (params, bindings)
        }
    };

    if params.len() != args.len() {
        return ir::Expr::Atom("opaque".into());
    }

    match type_decl {
        ast::TypeDecl::Record { name, fields, .. } => ir::Expr::Tuple(vec![
            ir::Expr::Atom("record".into()),
            ir::Expr::Atom(constructor_tag(name)),
            ir::Expr::Str(display_name(name)),
            ir::Expr::List(
                fields
                    .iter()
                    .map(|(field_name, field_ty)| {
                        ir::Expr::Tuple(vec![
                            ir::Expr::Str(field_name.clone()),
                            lower_debug_schema_from_usage(field_ty, &bindings, ctx, seen),
                        ])
                    })
                    .collect(),
            ),
        ]),
        ast::TypeDecl::Variant { constructors, .. } => ir::Expr::Tuple(vec![
            ir::Expr::Atom("variant".into()),
            ir::Expr::List(
                constructors
                    .iter()
                    .map(|(ctor_name, payloads)| {
                        if payloads.is_empty() {
                            return ir::Expr::Tuple(vec![
                                ir::Expr::Atom("atom".into()),
                                ir::Expr::Atom(constructor_tag(ctor_name)),
                                ir::Expr::Str(display_name(ctor_name)),
                            ]);
                        }

                        ir::Expr::Tuple(vec![
                            ir::Expr::Atom("tuple".into()),
                            ir::Expr::Atom(constructor_tag(ctor_name)),
                            ir::Expr::Str(display_name(ctor_name)),
                            ir::Expr::List(
                                payloads
                                    .iter()
                                    .map(|payload_ty| {
                                        lower_debug_schema_from_usage(
                                            payload_ty, &bindings, ctx, seen,
                                        )
                                    })
                                    .collect(),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ]),
    }
}

fn lower_debug_schema_from_usage(
    usage: &ast::TypeUsage,
    bindings: &HashMap<String, Arc<typecheck::Type>>,
    ctx: &Ctx,
    seen: &mut HashSet<String>,
) -> ir::Expr {
    instantiate_type_usage(usage, bindings)
        .map(|ty| lower_debug_schema(ty.as_ref(), ctx, seen))
        .unwrap_or_else(|| ir::Expr::Atom("opaque".into()))
}

fn instantiate_type_usage(
    usage: &ast::TypeUsage,
    bindings: &HashMap<String, Arc<typecheck::Type>>,
) -> Option<Arc<typecheck::Type>> {
    match usage {
        ast::TypeUsage::Named(name, _) => {
            Some(Arc::new(typecheck::Type::Con(name.clone(), vec![])))
        }
        ast::TypeUsage::Generic(name, _) => bindings.get(name).cloned(),
        ast::TypeUsage::App(name, args, _) => Some(Arc::new(typecheck::Type::Con(
            name.clone(),
            args.iter()
                .map(|arg| instantiate_type_usage(arg, bindings))
                .collect::<Option<Vec<_>>>()?,
        ))),
        ast::TypeUsage::Fun(_, _, _) => None,
    }
}

fn debug_support_source(compile_target: CompileTarget) -> String {
    let prefix = match compile_target {
        CompileTarget::Dev => DEBUG_DEV_SUPPORT,
        CompileTarget::Release => DEBUG_RELEASE_SUPPORT,
    };
    format!("{prefix}\n{DEBUG_SHARED_SUPPORT}")
}

const DEBUG_DEV_SUPPORT: &str = r##"
'mond$debug'(Value, Schema) ->
    io:put_chars(['mond$debug_render_dev'(Value, Schema, 24), $\n]),
    unit.

'mond$debug_render_dev'(_Value, _Schema, Depth) when Depth < 0 ->
    <<"#<debug depth-limit>">>;
'mond$debug_render_dev'(true, bool, _Depth) ->
    <<"True">>;
'mond$debug_render_dev'(false, bool, _Depth) ->
    <<"False">>;
'mond$debug_render_dev'(unit, unit, _Depth) ->
    <<"()">>;
'mond$debug_render_dev'(Int, int, _Depth) when is_integer(Int) ->
    integer_to_binary(Int);
'mond$debug_render_dev'(Float, float, _Depth) when is_float(Float) ->
    iolist_to_binary(io_lib:format("~p", [Float]));
'mond$debug_render_dev'(Binary, string, _Depth) when is_binary(Binary) ->
    case 'mond$debug_quote_utf8_binary'(Binary) of
        {ok, Quoted} -> Quoted;
        error -> iolist_to_binary(io_lib:format("~tp", [Binary]))
    end;
'mond$debug_render_dev'(List, {list, ElemSchema}, Depth) when is_list(List) ->
    case 'mond$debug_collect_list'(List, ElemSchema, Depth - 1, []) of
        {ok, Items} -> [$[, 'mond$debug_join_with_space'(Items), $]];
        improper -> 'mond$debug_render_release'(List, Depth)
    end;
'mond$debug_render_dev'(Tuple, {record, Tag, DisplayName, Fields}, Depth) when is_tuple(Tuple) ->
    case Tuple of
        _ when tuple_size(Tuple) =:= length(Fields) + 1, element(1, Tuple) =:= Tag ->
            'mond$debug_render_record'(DisplayName, Fields, tl(tuple_to_list(Tuple)), Depth - 1);
        _ ->
            'mond$debug_render_release'(Tuple, Depth)
    end;
'mond$debug_render_dev'(Atom, {variant, Constructors}, Depth) when is_atom(Atom) ->
    case 'mond$debug_find_variant_atom'(Atom, Constructors) of
        {ok, DisplayName} -> DisplayName;
        error -> 'mond$debug_render_release'(Atom, Depth)
    end;
'mond$debug_render_dev'(Tuple, {variant, Constructors}, Depth) when is_tuple(Tuple) ->
    case 'mond$debug_find_variant_tuple'(Tuple, Constructors) of
        {ok, DisplayName, PayloadSchemas} ->
            Values = tl(tuple_to_list(Tuple)),
            case 'mond$debug_render_variant_payloads'(Values, PayloadSchemas, Depth - 1) of
                {ok, PayloadRendered} -> [$(, DisplayName, $\s, PayloadRendered, $)];
                error -> 'mond$debug_render_release'(Tuple, Depth)
            end;
        error ->
            'mond$debug_render_release'(Tuple, Depth)
    end;
'mond$debug_render_dev'(Value, opaque, Depth) ->
    'mond$debug_render_release'(Value, Depth);
'mond$debug_render_dev'(Value, _Schema, Depth) ->
    'mond$debug_render_release'(Value, Depth).

'mond$debug_render_record'(DisplayName, Fields, Values, Depth) ->
    Parts = 'mond$debug_render_record_fields'(Fields, Values, Depth, []),
    case Parts of
        [] -> [$(, DisplayName, $)];
        _ -> [$(, DisplayName, $\s, 'mond$debug_join_with_space'(Parts), $)]
    end.

'mond$debug_render_record_fields'([], [], _Depth, Acc) ->
    lists:reverse(Acc);
'mond$debug_render_record_fields'([{FieldName, FieldSchema} | FieldRest], [Value | ValueRest], Depth, Acc) ->
    Rendered = 'mond$debug_render_dev'(Value, FieldSchema, Depth),
    'mond$debug_render_record_fields'(
        FieldRest,
        ValueRest,
        Depth,
        [[ $:, FieldName, $\s, Rendered ] | Acc]
    );
'mond$debug_render_record_fields'(_Fields, _Values, _Depth, _Acc) ->
    [].

'mond$debug_collect_list'([], _ElemSchema, _Depth, Acc) ->
    {ok, lists:reverse(Acc)};
'mond$debug_collect_list'([Head | Tail], ElemSchema, Depth, Acc) ->
    Rendered = 'mond$debug_render_dev'(Head, ElemSchema, Depth),
    'mond$debug_collect_list'(Tail, ElemSchema, Depth, [Rendered | Acc]);
'mond$debug_collect_list'(_Improper, _ElemSchema, _Depth, _Acc) ->
    improper.

'mond$debug_find_variant_atom'(_Atom, []) ->
    error;
'mond$debug_find_variant_atom'(Atom, [{atom, Atom, DisplayName} | _]) ->
    {ok, DisplayName};
'mond$debug_find_variant_atom'(Atom, [_ | Rest]) ->
    'mond$debug_find_variant_atom'(Atom, Rest).

'mond$debug_find_variant_tuple'(_Tuple, []) ->
    error;
'mond$debug_find_variant_tuple'(Tuple, [{tuple, Tag, DisplayName, PayloadSchemas} | _])
  when is_list(PayloadSchemas),
       tuple_size(Tuple) =:= length(PayloadSchemas) + 1,
       element(1, Tuple) =:= Tag ->
    {ok, DisplayName, PayloadSchemas};
'mond$debug_find_variant_tuple'(Tuple, [_ | Rest]) ->
    'mond$debug_find_variant_tuple'(Tuple, Rest).

'mond$debug_render_variant_payloads'(Values, Schemas, Depth) ->
    case 'mond$debug_render_variant_payload_items'(Values, Schemas, Depth, []) of
        {ok, Parts} -> {ok, 'mond$debug_join_with_space'(Parts)};
        error -> error
    end.

'mond$debug_render_variant_payload_items'([], [], _Depth, Acc) ->
    {ok, lists:reverse(Acc)};
'mond$debug_render_variant_payload_items'([Value | ValueRest], [Schema | SchemaRest], Depth, Acc) ->
    Rendered = 'mond$debug_render_dev'(Value, Schema, Depth),
    'mond$debug_render_variant_payload_items'(ValueRest, SchemaRest, Depth, [Rendered | Acc]);
'mond$debug_render_variant_payload_items'(_Values, _Schemas, _Depth, _Acc) ->
    error.
"##;

const DEBUG_RELEASE_SUPPORT: &str = r##"
'mond$debug'(Value) ->
    io:put_chars(['mond$debug_render_release'(Value, 24), $\n]),
    unit.
"##;

const DEBUG_SHARED_SUPPORT: &str = r##"
'mond$debug_render_release'(_Value, Depth) when Depth < 0 ->
    <<"#<debug depth-limit>">>;
'mond$debug_render_release'(true, _Depth) ->
    <<"True">>;
'mond$debug_render_release'(false, _Depth) ->
    <<"False">>;
'mond$debug_render_release'(unit, _Depth) ->
    <<"()">>;
'mond$debug_render_release'(Int, _Depth) when is_integer(Int) ->
    integer_to_binary(Int);
'mond$debug_render_release'(Float, _Depth) when is_float(Float) ->
    iolist_to_binary(io_lib:format("~p", [Float]));
'mond$debug_render_release'(Binary, _Depth) when is_binary(Binary) ->
    case 'mond$debug_quote_utf8_binary'(Binary) of
        {ok, Quoted} -> Quoted;
        error -> iolist_to_binary(io_lib:format("~tp", [Binary]))
    end;
'mond$debug_render_release'(List, Depth) when is_list(List) ->
    case 'mond$debug_collect_release_list'(List, Depth - 1, []) of
        {ok, Items} -> [$[, 'mond$debug_join_with_space'(Items), $]];
        improper -> iolist_to_binary(io_lib:format("~tp", [List]))
    end;
'mond$debug_render_release'(Value, _Depth) ->
    iolist_to_binary(io_lib:format("~tp", [Value])).

'mond$debug_collect_release_list'([], _Depth, Acc) ->
    {ok, lists:reverse(Acc)};
'mond$debug_collect_release_list'([Head | Tail], Depth, Acc) ->
    Rendered = 'mond$debug_render_release'(Head, Depth),
    'mond$debug_collect_release_list'(Tail, Depth, [Rendered | Acc]);
'mond$debug_collect_release_list'(_Improper, _Depth, _Acc) ->
    improper.

'mond$debug_quote_utf8_binary'(Binary) ->
    case catch unicode:characters_to_list(Binary, utf8) of
        Chars when is_list(Chars) ->
            {ok, [$", 'mond$debug_escape_chars'(Chars), $"]};
        _ ->
            error
    end.

'mond$debug_escape_chars'([]) ->
    [];
'mond$debug_escape_chars'([Char | Rest]) ->
    ['mond$debug_escape_char'(Char) | 'mond$debug_escape_chars'(Rest)].

'mond$debug_escape_char'($") ->
    <<"\\\"">>;
'mond$debug_escape_char'($\\) ->
    <<"\\\\">>;
'mond$debug_escape_char'($\n) ->
    <<"\\n">>;
'mond$debug_escape_char'($\t) ->
    <<"\\t">>;
'mond$debug_escape_char'($\r) ->
    <<"\\r">>;
'mond$debug_escape_char'(Char) when Char < 32; Char =:= 127 ->
    ["\\u{", string:uppercase(integer_to_list(Char, 16)), "}"];
'mond$debug_escape_char'(Char) when Char =< 255 ->
    Char;
'mond$debug_escape_char'(Char) ->
    unicode:characters_to_binary([Char]).

'mond$debug_join_with_space'([]) ->
    [];
'mond$debug_join_with_space'([Item]) ->
    Item;
'mond$debug_join_with_space'([Item | Rest]) ->
    [Item, $\s | 'mond$debug_join_with_space'(Rest)].
"##;

// ─── Expression lowering ────────────────────────────────────────────────────

fn lower_expr(expr: &ast::Expr, ctx: &Ctx) -> ir::Expr {
    let renames = HashMap::new();
    let mut fresh_idx = 0usize;
    lower_expr_with_renames(expr, ctx, &renames, &mut fresh_idx)
}

fn lower_expr_with_renames(
    expr: &ast::Expr,
    ctx: &Ctx,
    renames: &HashMap<String, String>,
    fresh_idx: &mut usize,
) -> ir::Expr {
    match expr {
        ast::Expr::Literal(lit, _) => lower_literal(lit),

        ast::Expr::Variable(name, _) => lower_variable(name, ctx, renames),

        ast::Expr::List(items, _) => ir::Expr::List(
            items
                .iter()
                .map(|e| lower_expr_with_renames(e, ctx, renames, fresh_idx))
                .collect(),
        ),

        ast::Expr::LetLocal {
            name, value, body, ..
        } => {
            let value_ir = lower_expr_with_renames(value, ctx, renames, fresh_idx);
            let mut body_renames = renames.clone();
            let binding_var = if name == "_" {
                "_".to_string()
            } else {
                let fresh = format!("{}__l{}", var_name(name), *fresh_idx);
                *fresh_idx += 1;
                fresh
            };
            if name == "_" {
                body_renames.remove(name);
            } else {
                body_renames.insert(name.clone(), binding_var.clone());
            }
            let body_ir = lower_expr_with_renames(body, ctx, &body_renames, fresh_idx);
            ir::Expr::Let(binding_var, Box::new(value_ir), Box::new(body_ir))
        }

        ast::Expr::If {
            cond, then, els, ..
        } => ir::Expr::Case(
            Box::new(lower_expr_with_renames(cond, ctx, renames, fresh_idx)),
            vec![
                (
                    ir::Pattern::Atom("true".into()),
                    lower_expr_with_renames(then, ctx, renames, fresh_idx),
                ),
                (
                    ir::Pattern::Any,
                    lower_expr_with_renames(els, ctx, renames, fresh_idx),
                ),
            ],
        ),

        ast::Expr::Debug { value, .. } => {
            let value_ir = lower_expr_with_renames(value, ctx, renames, fresh_idx);
            match ctx.compile_target {
                CompileTarget::Dev => ir::Expr::LocalCallMulti(
                    DEBUG_HELPER_NAME.to_string(),
                    vec![value_ir, debug_schema_for_expr(value, ctx)],
                ),
                CompileTarget::Release => {
                    ir::Expr::LocalCallMulti(DEBUG_HELPER_NAME.to_string(), vec![value_ir])
                }
            }
        }

        ast::Expr::Match { targets, arms, .. } => {
            let scrutinee = if targets.len() == 1 {
                lower_expr_with_renames(&targets[0], ctx, renames, fresh_idx)
            } else {
                ir::Expr::Tuple(
                    targets
                        .iter()
                        .map(|t| lower_expr_with_renames(t, ctx, renames, fresh_idx))
                        .collect(),
                )
            };

            if arms.iter().any(|arm| arm.guard.is_some()) {
                let scrutinee_var = format!("MatchScrut{}__", *fresh_idx);
                *fresh_idx += 1;
                let scrutinee_ref = ir::Expr::Var(scrutinee_var.clone());
                let clauses = lower_match_case_clauses(
                    targets.len(),
                    arms,
                    &scrutinee_ref,
                    ctx,
                    renames,
                    fresh_idx,
                );
                let case_expr = ir::Expr::Case(Box::new(scrutinee_ref.clone()), clauses);
                return ir::Expr::Let(scrutinee_var, Box::new(scrutinee), Box::new(case_expr));
            }

            let mut ir_arms = Vec::new();
            for arm in arms {
                let mut arm_renames = renames.clone();
                let mut pattern_renames: HashMap<String, String> = HashMap::new();
                for pat in &arm.patterns {
                    let mut names = Vec::new();
                    collect_pattern_vars(pat, &mut names);
                    for name in names {
                        if let std::collections::hash_map::Entry::Vacant(v) =
                            pattern_renames.entry(name.clone())
                        {
                            let fresh = format!("{}__p{}", var_name(&name), *fresh_idx);
                            *fresh_idx += 1;
                            v.insert(fresh);
                        }
                    }
                }
                arm_renames.extend(pattern_renames.clone());
                let body_ir = lower_expr_with_renames(&arm.body, ctx, &arm_renames, fresh_idx);
                if targets.len() == 1 {
                    for pat in top_level_pattern_alternatives(&arm.patterns[0]) {
                        ir_arms.push((lower_pattern(pat, ctx, &pattern_renames), body_ir.clone()));
                    }
                } else {
                    let pat = ir::Pattern::Tuple(
                        arm.patterns
                            .iter()
                            .map(|p| lower_pattern(p, ctx, &pattern_renames))
                            .collect(),
                    );
                    ir_arms.push((pat, body_ir));
                }
            }

            ir::Expr::Case(Box::new(scrutinee), ir_arms)
        }

        ast::Expr::Lambda { args, body, .. } => {
            let body_renames = extend_renames_with_binders(renames, args);
            let body_ir = lower_expr_with_renames(body, ctx, &body_renames, fresh_idx);
            make_lambda(args, body_ir)
        }

        ast::Expr::Call { func, args, .. } => lower_call(func, args, ctx, renames, fresh_idx),

        ast::Expr::FieldAccess { field, record, .. } => {
            if let Some(idx) = record_name_for_expr(record, ctx)
                .and_then(|record_name| field_index_for_record(ctx, record_name, field))
            {
                ir::Expr::RemoteCall(
                    "erlang".into(),
                    "element".into(),
                    vec![
                        ir::Expr::Int(idx as i64),
                        lower_expr_with_renames(record, ctx, renames, fresh_idx),
                    ],
                )
            } else {
                dynamic_field_access(
                    field,
                    lower_expr_with_renames(record, ctx, renames, fresh_idx),
                    ctx,
                    fresh_idx,
                )
            }
        }

        ast::Expr::RecordConstruct { name, fields, .. } => {
            // {name, field1, field2, ...} — fields in declaration order
            let tag = ir::Expr::Atom(constructor_tag(name));
            let mut items = vec![tag];
            if let Some(layout) = ctx.record_layouts.get(name) {
                let by_name: HashMap<String, &ast::Expr> = fields
                    .iter()
                    .map(|(field, expr)| (field.clone(), expr))
                    .collect();
                let has_duplicates = by_name.len() != fields.len();
                let has_unknown = fields
                    .iter()
                    .any(|(field, _)| !layout.iter().any(|declared| declared == field));
                let has_missing = layout.iter().any(|field| !by_name.contains_key(field));

                if !has_duplicates && !has_unknown && !has_missing {
                    for field in layout {
                        let expr = by_name
                            .get(field)
                            .expect("validated record layout field exists");
                        items.push(lower_expr_with_renames(expr, ctx, renames, fresh_idx));
                    }
                } else {
                    items.extend(
                        fields
                            .iter()
                            .map(|(_, e)| lower_expr_with_renames(e, ctx, renames, fresh_idx)),
                    );
                }
            } else {
                items.extend(
                    fields
                        .iter()
                        .map(|(_, e)| lower_expr_with_renames(e, ctx, renames, fresh_idx)),
                );
            }
            ir::Expr::Tuple(items)
        }

        ast::Expr::RecordUpdate {
            record, updates, ..
        } => {
            // Evaluate the base record exactly once, then apply updates via setelement/3.
            let record_var = format!("RUpd{}__", *fresh_idx);
            *fresh_idx += 1;
            let base_ir = lower_expr_with_renames(record, ctx, renames, fresh_idx);
            let updated_ir =
                updates
                    .iter()
                    .fold(ir::Expr::Var(record_var.clone()), |acc, (field, value)| {
                        let value_ir = lower_expr_with_renames(value, ctx, renames, fresh_idx);
                        if let Some(idx) = record_name_for_expr(record, ctx)
                            .and_then(|record_name| field_index_for_record(ctx, record_name, field))
                        {
                            ir::Expr::RemoteCall(
                                "erlang".into(),
                                "setelement".into(),
                                vec![ir::Expr::Int(idx as i64), acc, value_ir],
                            )
                        } else {
                            dynamic_record_update(field, acc, value_ir, ctx, fresh_idx)
                        }
                    });
            ir::Expr::Let(record_var, Box::new(base_ir), Box::new(updated_ir))
        }

        ast::Expr::QualifiedCall {
            module,
            function,
            args,
            ..
        } => {
            let constructor_name = format!("{module}/{function}");
            if let Some(&arity) = ctx.constructors.get(&constructor_name) {
                if arity == 0 && args.is_empty() {
                    return ir::Expr::Atom(constructor_tag(&constructor_name));
                }
                if arity > 0 {
                    let tag = ir::Expr::Atom(constructor_tag(&constructor_name));
                    let mut items = vec![tag];
                    items.extend(
                        args.iter()
                            .map(|a| lower_expr_with_renames(a, ctx, renames, fresh_idx)),
                    );
                    return ir::Expr::Tuple(items);
                }
            }

            let erl_module = ctx
                .module_aliases
                .get(module.as_str())
                .cloned()
                .unwrap_or_else(|| module.clone());
            if args.is_empty() {
                // 0-arg: true Erlang 0-arity call
                ir::Expr::RemoteCall(erl_module, function.clone(), vec![])
            } else {
                // First arg goes into the remote call, rest chain as curried calls
                let first = lower_expr_with_renames(&args[0], ctx, renames, fresh_idx);
                let mut result = ir::Expr::RemoteCall(erl_module, function.clone(), vec![first]);
                for arg in &args[1..] {
                    result = ir::Expr::Call(
                        Box::new(result),
                        Box::new(lower_expr_with_renames(arg, ctx, renames, fresh_idx)),
                    );
                }
                result
            }
        }

        ast::Expr::LetFunc { .. } => unreachable!("LetFunc only at top level"),
    }
}

fn lower_call(
    func: &ast::Expr,
    args: &[ast::Expr],
    ctx: &Ctx,
    renames: &HashMap<String, String>,
    fresh_idx: &mut usize,
) -> ir::Expr {
    // Saturated curried call chain over a known local multi-arg function:
    // ((f a) b) -> f(a, b)
    // This keeps recursive calls on the direct N-arity path for BEAM TCO.
    if let Some((name, flattened_args)) = flatten_call_chain(func, args)
        && ctx.fn_names.contains(name)
    {
        let mond_arity = ctx.fn_arities.get(name).copied().unwrap_or(0);
        if mond_arity >= 2 && flattened_args.len() == mond_arity {
            let lowered = flattened_args
                .iter()
                .map(|a| lower_expr_with_renames(a, ctx, renames, fresh_idx))
                .collect();
            return ir::Expr::LocalCallMulti(name.to_string(), lowered);
        }
    }

    if let ast::Expr::Variable(name, _) = func {
        // Constructor application: Ok x / option/Some x -> {ok, X}
        if let Some(&arity) = ctx.constructors.get(name.as_str())
            && arity > 0
        {
            let tag = ir::Expr::Atom(constructor_tag(name));
            let mut items = vec![tag];
            items.extend(
                args.iter()
                    .map(|a| lower_expr_with_renames(a, ctx, renames, fresh_idx)),
            );
            return ir::Expr::Tuple(items);
        }

        // Qualified function variable (`module/function`) in call position.
        if let Some((module, function)) = name.split_once('/') {
            let erl_module = ctx
                .module_aliases
                .get(module)
                .cloned()
                .unwrap_or_else(|| module.to_string());
            if args.is_empty() {
                return ir::Expr::RemoteCall(erl_module, function.to_string(), vec![]);
            }
            let first = lower_expr_with_renames(&args[0], ctx, renames, fresh_idx);
            let mut result = ir::Expr::RemoteCall(erl_module, function.to_string(), vec![first]);
            for arg in &args[1..] {
                result = ir::Expr::Call(
                    Box::new(result),
                    Box::new(lower_expr_with_renames(arg, ctx, renames, fresh_idx)),
                );
            }
            return result;
        }

        // Binary operator
        if args.len() == 2
            && let Some(erl_op) = binary_op(name)
        {
            return ir::Expr::BinOp(
                erl_op.to_string(),
                Box::new(lower_expr_with_renames(&args[0], ctx, renames, fresh_idx)),
                Box::new(lower_expr_with_renames(&args[1], ctx, renames, fresh_idx)),
            );
        }

        // Unary operator
        if args.len() == 1
            && let Some(erl_op) = unary_op(name)
        {
            return ir::Expr::UnOp(
                erl_op.to_string(),
                Box::new(lower_expr_with_renames(&args[0], ctx, renames, fresh_idx)),
            );
        }

        // Imported function via `use` — emit as remote call
        if let Some(module) = ctx.imports.get(name.as_str()) {
            let module = module.clone();
            if args.is_empty() {
                return ir::Expr::RemoteCall(module, name.clone(), vec![]);
            }
            let first = lower_expr_with_renames(&args[0], ctx, renames, fresh_idx);
            let mut result = ir::Expr::RemoteCall(module, name.clone(), vec![first]);
            for arg in &args[1..] {
                result = ir::Expr::Call(
                    Box::new(result),
                    Box::new(lower_expr_with_renames(arg, ctx, renames, fresh_idx)),
                );
            }
            return result;
        }

        // Known local function — emit direct call
        if ctx.fn_names.contains(name.as_str()) {
            if args.is_empty() {
                if ctx.nullary_externs.contains(name.as_str()) {
                    return ir::Expr::LocalCallMulti(name.clone(), vec![]);
                }
                // 0-arg call → call with unit
                return ir::Expr::LocalCall(name.clone(), Box::new(ir::Expr::Atom("unit".into())));
            }
            let mond_arity = ctx.fn_arities.get(name.as_str()).copied().unwrap_or(0);
            // Full application of a multi-arg function → direct N-arity call (enables TCO)
            if mond_arity >= 2 && args.len() == mond_arity {
                let lowered = args
                    .iter()
                    .map(|a| lower_expr_with_renames(a, ctx, renames, fresh_idx))
                    .collect();
                return ir::Expr::LocalCallMulti(name.clone(), lowered);
            }
            // Partial application or single-arg — use curried form
            let first = lower_expr_with_renames(&args[0], ctx, renames, fresh_idx);
            let mut result = ir::Expr::LocalCall(name.clone(), Box::new(first));
            for arg in &args[1..] {
                result = ir::Expr::Call(
                    Box::new(result),
                    Box::new(lower_expr_with_renames(arg, ctx, renames, fresh_idx)),
                );
            }
            return result;
        }
    }

    // General curried application: chain args left to right
    // 0-arg call on an arbitrary expr → call with unit
    if args.is_empty() {
        return ir::Expr::Call(
            Box::new(lower_expr_with_renames(func, ctx, renames, fresh_idx)),
            Box::new(ir::Expr::Atom("unit".into())),
        );
    }
    let mut result = lower_expr_with_renames(func, ctx, renames, fresh_idx);
    for arg in args {
        result = ir::Expr::Call(
            Box::new(result),
            Box::new(lower_expr_with_renames(arg, ctx, renames, fresh_idx)),
        );
    }
    result
}

fn flatten_call_chain<'a>(
    func: &'a ast::Expr,
    args: &'a [ast::Expr],
) -> Option<(&'a str, Vec<&'a ast::Expr>)> {
    let mut arg_segments: Vec<&'a [ast::Expr]> = vec![args];
    let mut current = func;

    loop {
        match current {
            ast::Expr::Variable(name, _) => {
                let mut flat_args = Vec::new();
                for segment in arg_segments.iter().rev() {
                    flat_args.extend(segment.iter());
                }
                return Some((name.as_str(), flat_args));
            }
            ast::Expr::Call {
                func: inner_func,
                args: inner_args,
                ..
            } => {
                arg_segments.push(inner_args);
                current = inner_func.as_ref();
            }
            _ => return None,
        }
    }
}

fn lower_variable(name: &str, ctx: &Ctx, renames: &HashMap<String, String>) -> ir::Expr {
    if let Some(mapped) = renames.get(name) {
        return ir::Expr::Var(mapped.clone());
    }
    if let Some(erl_op) = unary_op(name) {
        return ir::Expr::Fun(
            "A__".to_string(),
            Box::new(ir::Expr::UnOp(
                erl_op.to_string(),
                Box::new(ir::Expr::Var("A__".to_string())),
            )),
        );
    }
    if let Some(erl_op) = binary_op(name) {
        return ir::Expr::Fun(
            "A__".to_string(),
            Box::new(ir::Expr::Fun(
                "B__".to_string(),
                Box::new(ir::Expr::BinOp(
                    erl_op.to_string(),
                    Box::new(ir::Expr::Var("A__".to_string())),
                    Box::new(ir::Expr::Var("B__".to_string())),
                )),
            )),
        );
    }
    // Nullary constructor → atom
    if let Some(&0) = ctx.constructors.get(name) {
        return ir::Expr::Atom(constructor_tag(name));
    }
    // Non-nullary constructor in value position → curried lambda: fun(X0) -> {tag, X0} end
    if let Some(&arity) = ctx.constructors.get(name) {
        let tag = ir::Expr::Atom(constructor_tag(name));
        let params: Vec<String> = (0..arity).map(|i| format!("X{i}__")).collect();
        let mut items = vec![tag];
        items.extend(params.iter().map(|p| ir::Expr::Var(p.clone())));
        let body = ir::Expr::Tuple(items);
        return params
            .iter()
            .rev()
            .fold(body, |acc, p| ir::Expr::Fun(p.clone(), Box::new(acc)));
    }
    // Top-level function in value position → fun f/1
    if ctx.fn_names.contains(name) {
        return ir::Expr::FunRef(name.to_string());
    }
    // Imported function in value position → fun module:f/1
    if let Some(module) = ctx.imports.get(name) {
        return ir::Expr::RemoteFunRef(module.clone(), name.to_string());
    }
    // Qualified function in value position → fun module:function/1
    if let Some((module, function)) = name.split_once('/') {
        let erl_module = ctx
            .module_aliases
            .get(module)
            .cloned()
            .unwrap_or_else(|| module.to_string());
        return ir::Expr::RemoteFunRef(erl_module, function.to_string());
    }
    // Local variable → capitalize
    ir::Expr::Var(var_name(name))
}

fn lower_literal(lit: &ast::Literal) -> ir::Expr {
    match lit {
        ast::Literal::Int(n) => ir::Expr::Int(*n),
        ast::Literal::Float(f) => ir::Expr::Float(*f),
        ast::Literal::Bool(b) => ir::Expr::Atom(if *b { "true" } else { "false" }.into()),
        ast::Literal::String(s) => ir::Expr::Str(s.clone()),
        ast::Literal::Unit => ir::Expr::Atom("unit".into()),
    }
}

fn lower_pattern(pat: &ast::Pattern, ctx: &Ctx, renames: &HashMap<String, String>) -> ir::Pattern {
    match pat {
        ast::Pattern::Any(_) => ir::Pattern::Any,
        ast::Pattern::Variable(name, _) => renames
            .get(name)
            .cloned()
            .map(ir::Pattern::Var)
            .unwrap_or_else(|| ir::Pattern::Var(var_name(name))),
        ast::Pattern::Literal(lit, _) => match lit {
            ast::Literal::Int(n) => ir::Pattern::Int(*n),
            ast::Literal::Float(f) => ir::Pattern::Float(*f),
            ast::Literal::Bool(b) => ir::Pattern::Atom(if *b { "true" } else { "false" }.into()),
            ast::Literal::String(s) => ir::Pattern::Str(s.clone()),
            ast::Literal::Unit => ir::Pattern::Atom("unit".into()),
        },
        ast::Pattern::Constructor(name, sub_pats, _) => {
            let arity = ctx.constructors.get(name.as_str()).copied().unwrap_or(0);
            if arity == 0 {
                ir::Pattern::Atom(constructor_tag(name))
            } else {
                let tag = ir::Pattern::Atom(constructor_tag(name));
                let mut items = vec![tag];
                items.extend(sub_pats.iter().map(|p| lower_pattern(p, ctx, renames)));
                ir::Pattern::Tuple(items)
            }
        }
        ast::Pattern::EmptyList(_) => ir::Pattern::List(vec![]),

        ast::Pattern::Cons(head, tail, _) => ir::Pattern::Cons(
            Box::new(lower_pattern(head, ctx, renames)),
            Box::new(lower_pattern(tail, ctx, renames)),
        ),

        ast::Pattern::Record { name, fields, .. } => {
            let mut by_name = HashMap::new();
            for (field_name, pat, _) in fields {
                by_name.insert(field_name.as_str(), pat);
            }

            let mut items = vec![ir::Pattern::Atom(constructor_tag(name))];
            if let Some(layout) = ctx.record_layouts.get(name) {
                for field_name in layout {
                    let pat = by_name
                        .get(field_name.as_str())
                        .map(|pat| lower_pattern(pat, ctx, renames))
                        .unwrap_or(ir::Pattern::Any);
                    items.push(pat);
                }
            }
            ir::Pattern::Tuple(items)
        }

        ast::Pattern::Or(pats, _) => {
            // Or-patterns are expanded by the caller via expand_or_pattern
            // If we get here directly, just use the first alternative
            lower_pattern(&pats[0], ctx, renames)
        }
    }
}

fn lower_match_case_clauses(
    targets_len: usize,
    arms: &[ast::MatchArm],
    scrutinee_ref: &ir::Expr,
    ctx: &Ctx,
    renames: &HashMap<String, String>,
    fresh_idx: &mut usize,
) -> Vec<(ir::Pattern, ir::Expr)> {
    let Some(first) = arms.first() else {
        return vec![(ir::Pattern::Any, match_case_clause_error(scrutinee_ref))];
    };

    let rest = lower_match_case_clauses(
        targets_len,
        &arms[1..],
        scrutinee_ref,
        ctx,
        renames,
        fresh_idx,
    );

    let mut arm_renames = renames.clone();
    let mut pattern_renames: HashMap<String, String> = HashMap::new();
    for pat in &first.patterns {
        let mut names = Vec::new();
        collect_pattern_vars(pat, &mut names);
        for name in names {
            if let std::collections::hash_map::Entry::Vacant(v) =
                pattern_renames.entry(name.clone())
            {
                let fresh = format!("{}__p{}", var_name(&name), *fresh_idx);
                *fresh_idx += 1;
                v.insert(fresh);
            }
        }
    }
    arm_renames.extend(pattern_renames.clone());

    let body_ir = lower_expr_with_renames(&first.body, ctx, &arm_renames, fresh_idx);
    let guarded_body = if let Some(guard) = &first.guard {
        let guard_ir = lower_expr_with_renames(guard, ctx, &arm_renames, fresh_idx);
        let fallback = ir::Expr::Case(Box::new(scrutinee_ref.clone()), rest.clone());
        ir::Expr::Case(
            Box::new(guard_ir),
            vec![
                (ir::Pattern::Atom("true".into()), body_ir),
                (ir::Pattern::Any, fallback),
            ],
        )
    } else {
        body_ir
    };

    let mut clauses = Vec::new();
    if targets_len == 1 {
        for pat in top_level_pattern_alternatives(&first.patterns[0]) {
            clauses.push((
                lower_pattern(pat, ctx, &pattern_renames),
                guarded_body.clone(),
            ));
        }
    } else {
        let pat = ir::Pattern::Tuple(
            first
                .patterns
                .iter()
                .map(|p| lower_pattern(p, ctx, &pattern_renames))
                .collect(),
        );
        clauses.push((pat, guarded_body));
    }
    clauses.extend(rest);
    clauses
}

fn match_case_clause_error(scrutinee_ref: &ir::Expr) -> ir::Expr {
    ir::Expr::RemoteCall(
        "erlang".into(),
        "error".into(),
        vec![ir::Expr::Tuple(vec![
            ir::Expr::Atom("case_clause".into()),
            scrutinee_ref.clone(),
        ])],
    )
}

fn collect_pattern_vars(pat: &ast::Pattern, out: &mut Vec<String>) {
    match pat {
        ast::Pattern::Variable(name, _) => {
            if !out.contains(name) {
                out.push(name.clone());
            }
        }
        ast::Pattern::Constructor(_, args, _) => {
            for arg in args {
                collect_pattern_vars(arg, out);
            }
        }
        ast::Pattern::Or(pats, _) => {
            for p in pats {
                collect_pattern_vars(p, out);
            }
        }
        ast::Pattern::Cons(head, tail, _) => {
            collect_pattern_vars(head, out);
            collect_pattern_vars(tail, out);
        }
        ast::Pattern::Record { fields, .. } => {
            for (_, pat, _) in fields {
                collect_pattern_vars(pat, out);
            }
        }
        ast::Pattern::Any(_) | ast::Pattern::Literal(_, _) | ast::Pattern::EmptyList(_) => {}
    }
}

fn top_level_pattern_alternatives(pat: &ast::Pattern) -> Vec<&ast::Pattern> {
    match pat {
        ast::Pattern::Or(pats, _) => pats.iter().collect(),
        _ => vec![pat],
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Build a curried lambda: `(f {a b c} -> body)` → `fun(A) -> fun(B) -> fun(C) -> body end end end`
fn make_lambda(args: &[String], body: ir::Expr) -> ir::Expr {
    if args.is_empty() {
        ir::Expr::Fun("_Unit".to_string(), Box::new(body))
    } else {
        // Fold from right: innermost arg wraps body first
        args.iter()
            .rev()
            .fold(body, |acc, arg| ir::Expr::Fun(var_name(arg), Box::new(acc)))
    }
}

fn extend_renames_with_binders(
    base: &HashMap<String, String>,
    binders: &[String],
) -> HashMap<String, String> {
    let mut out = base.clone();
    for binder in binders {
        if binder == "_" {
            out.remove(binder);
        } else {
            out.insert(binder.clone(), var_name(binder));
        }
    }
    out
}

fn lower_function_body(args: &[String], body: &ast::Expr, ctx: &Ctx) -> ir::Expr {
    let renames = extend_renames_with_binders(&HashMap::new(), args);
    let mut fresh_idx = 0usize;
    lower_expr_with_renames(body, ctx, &renames, &mut fresh_idx)
}

/// Capitalize first character of a variable name for Erlang.
/// `x` → `X`, `my_var` → `My_var`, `_` → `_`
fn var_name(name: &str) -> String {
    if name == "_" {
        return "_".to_string();
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

fn type_sig_arity(ty: &ast::TypeSig) -> usize {
    match ty {
        ast::TypeSig::Fun(_, rest) => 1 + type_sig_arity(rest),
        _ => 0,
    }
}

fn binary_op(name: &str) -> Option<&'static str> {
    match name {
        "+" => Some("+"),
        "-" => Some("-"),
        "*" => Some("*"),
        "/" => Some("div"),
        "%" => Some("rem"),
        "+." => Some("+"),
        "-." => Some("-"),
        "*." => Some("*"),
        "/." => Some("/"),
        "=" => Some("=:="),
        "!=" => Some("=/="),
        "<" => Some("<"),
        ">" => Some(">"),
        "<=" => Some("=<"),
        ">=" => Some(">="),
        "and" => Some("andalso"),
        "or" => Some("orelse"),
        _ => None,
    }
}

fn unary_op(name: &str) -> Option<&'static str> {
    match name {
        "not" => Some("not"),
        _ => None,
    }
}

// ─── Emitter: IR → Erlang source ────────────────────────────────────────────

/// Quote an Erlang atom if it doesn't match `[a-z][a-zA-Z0-9_]*`.
fn quote_atom(name: &str) -> String {
    let needs_quoting = name.starts_with(|c: char| !c.is_lowercase())
        || name.chars().any(|c| !c.is_alphanumeric() && c != '_')
        || is_erlang_reserved_atom(name);
    if needs_quoting {
        format!("'{name}'")
    } else {
        name.to_string()
    }
}

fn is_erlang_reserved_atom(name: &str) -> bool {
    matches!(
        name,
        "after"
            | "and"
            | "andalso"
            | "band"
            | "begin"
            | "bnot"
            | "bor"
            | "bsl"
            | "bsr"
            | "bxor"
            | "case"
            | "catch"
            | "cond"
            | "div"
            | "end"
            | "fun"
            | "if"
            | "let"
            | "not"
            | "of"
            | "or"
            | "orelse"
            | "receive"
            | "rem"
            | "try"
            | "when"
            | "xor"
    )
}

pub fn emit_module(module: &ir::Module) -> String {
    let mut out = String::new();

    out.push_str(&format!("-module({}).\n", module.name));

    // Export all functions. Mond privacy is enforced by the import system, not Erlang exports.
    let exports: Vec<String> = module
        .functions
        .iter()
        .map(|f| format!("{}/{}", quote_atom(&f.name), f.params.len()))
        .collect();
    out.push_str(&format!("-export([{}]).\n\n", exports.join(", ")));

    for func in &module.functions {
        out.push_str(&emit_function(func));
        out.push('\n');
    }

    for source in &module.support_sources {
        out.push_str(source.trim_start_matches('\n'));
        out.push('\n');
    }

    out
}

fn emit_function(func: &ir::Function) -> String {
    let params_s = func.params.join(", ");
    // Flatten top-level Let chain into a statement list for clean output:
    //   X = val,
    //   Y = ...,
    //   final_expr.
    let (bindings, final_expr) = collect_lets(&func.body);
    let body_s = if bindings.is_empty() {
        emit_expr(final_expr)
    } else {
        let mut parts: Vec<String> = bindings
            .iter()
            .map(|(v, e)| format!("{v} = {}", emit_expr(e)))
            .collect();
        parts.push(emit_expr(final_expr));
        parts.join(",\n    ")
    };
    format!(
        "{}({params_s}) ->\n    {}.\n",
        quote_atom(&func.name),
        body_s
    )
}

/// Peel off consecutive `Let` nodes, returning the list of `(var, val)` bindings
/// and the final non-Let expression.
fn collect_lets(expr: &ir::Expr) -> (Vec<(&str, &ir::Expr)>, &ir::Expr) {
    let mut bindings = Vec::new();
    let mut cur = expr;
    while let ir::Expr::Let(var, val, body) = cur {
        bindings.push((var.as_str(), val.as_ref()));
        cur = body.as_ref();
    }
    (bindings, cur)
}

fn emit_expr(expr: &ir::Expr) -> String {
    match expr {
        ir::Expr::Atom(s) => quote_atom(s),
        ir::Expr::Int(n) => n.to_string(),
        ir::Expr::Float(f) => format!("{f}"),
        ir::Expr::Str(s) => format!("<<\"{}\"/utf8>>", escape_str(s)),
        ir::Expr::Var(s) => s.clone(),
        ir::Expr::FunRef(name) => format!("fun {}/1", quote_atom(name)),
        ir::Expr::RemoteFunRef(module, name) => {
            format!("fun {}:{}/1", quote_atom(module), quote_atom(name))
        }

        ir::Expr::Tuple(items) => {
            format!(
                "{{{}}}",
                items.iter().map(emit_expr).collect::<Vec<_>>().join(", ")
            )
        }

        ir::Expr::List(items) => {
            if items.is_empty() {
                "[]".to_string()
            } else {
                format!(
                    "[{}]",
                    items.iter().map(emit_expr).collect::<Vec<_>>().join(", ")
                )
            }
        }

        ir::Expr::Fun(param, body) => {
            format!("fun({param}) -> {body} end", body = emit_expr(body))
        }

        ir::Expr::Call(func, arg) => {
            let arg_s = emit_expr(arg);
            match func.as_ref() {
                // Simple expressions can be called directly
                ir::Expr::Var(name) | ir::Expr::Atom(name) => format!("{name}({arg_s})"),
                ir::Expr::FunRef(name) => format!("(fun {name}/1)({arg_s})"),
                // Chained calls need parens around the inner call
                ir::Expr::Call(_, _)
                | ir::Expr::LocalCall(_, _)
                | ir::Expr::LocalCallMulti(_, _) => {
                    format!("({})({})", emit_expr(func), arg_s)
                }
                other => format!("({})({})", emit_expr(other), arg_s),
            }
        }

        ir::Expr::LocalCall(name, arg) => {
            format!("{}({})", quote_atom(name), emit_expr(arg))
        }

        ir::Expr::LocalCallMulti(name, args) => {
            let args_s = args.iter().map(emit_expr).collect::<Vec<_>>().join(", ");
            format!("{}({})", quote_atom(name), args_s)
        }

        ir::Expr::RemoteCall(module, function, args) => {
            let args_s = args.iter().map(emit_expr).collect::<Vec<_>>().join(", ");
            format!("{}:{}({args_s})", quote_atom(module), quote_atom(function))
        }

        ir::Expr::BinOp(op, lhs, rhs) => {
            format!("({} {op} {})", emit_expr(lhs), emit_expr(rhs))
        }

        ir::Expr::UnOp(op, expr) => {
            format!("({op} {})", emit_expr(expr))
        }

        ir::Expr::Let(_, _, _) => {
            // Flatten consecutive Lets and wrap in begin...end (valid in any expression position)
            let (bindings, final_expr) = collect_lets(expr);
            let mut parts: Vec<String> = bindings
                .iter()
                .map(|(v, e)| format!("{v} = {}", emit_expr(e)))
                .collect();
            parts.push(emit_expr(final_expr));
            format!("begin {} end", parts.join(", "))
        }

        ir::Expr::Case(scrutinee, arms) => {
            let arms_s: Vec<String> = arms
                .iter()
                .map(|(pat, body)| format!("        {} -> {}", emit_pattern(pat), emit_expr(body)))
                .collect();
            format!(
                "case {} of\n{}\n    end",
                emit_expr(scrutinee),
                arms_s.join(";\n")
            )
        }
    }
}

fn escape_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn emit_pattern(pat: &ir::Pattern) -> String {
    match pat {
        ir::Pattern::Any => "_".to_string(),
        ir::Pattern::Var(s) => s.clone(),
        ir::Pattern::Atom(s) => quote_atom(s),
        ir::Pattern::Int(n) => n.to_string(),
        ir::Pattern::Float(f) => format!("{f}"),
        ir::Pattern::Str(s) => format!("<<\"{}\"/utf8>>", escape_str(s)),
        ir::Pattern::Tuple(items) => {
            format!(
                "{{{}}}",
                items
                    .iter()
                    .map(emit_pattern)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        ir::Pattern::List(items) => {
            if items.is_empty() {
                "[]".to_string()
            } else {
                format!(
                    "[{}]",
                    items
                        .iter()
                        .map(emit_pattern)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
        ir::Pattern::Cons(head, tail) => {
            format!("[{} | {}]", emit_pattern(head), emit_pattern(tail))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{emit_expr, emit_pattern};
    use crate::ir;

    #[test]
    fn modules_without_debug_do_not_emit_debug_helpers_or_type_info() {
        let erl = crate::compile("test", "(let main {} 1)").unwrap();
        assert!(
            !erl.contains("mond_type_info"),
            "did not expect mond_type_info metadata:\n{erl}"
        );
        assert!(
            !erl.contains("'mond$debug'"),
            "did not expect debug helper:\n{erl}"
        );
    }

    #[test]
    fn debug_uses_schema_driven_helpers_in_dev_builds() {
        let src = r#"
(type Failure [InvalidPid (BadStatus ~ Int)])
(type RetryState [(:attempt ~ Int) (:retry_after ~ Int)])
(let main {}
  (debug (RetryState :attempt 1 :retry_after 5)))
"#;
        let erl = crate::compile("test", src).unwrap();
        assert!(
            erl.contains("'mond$debug'("),
            "expected debug helper call:\n{erl}"
        );
        assert!(
            erl.contains("{record, retrystate, <<\"RetryState\"/utf8>>"),
            "expected record schema in emitted debug call:\n{erl}"
        );
        assert!(
            !erl.contains("mond_type_info"),
            "did not expect old module metadata path:\n{erl}"
        );
    }

    #[test]
    fn multi_arg_tco_emits_two_functions() {
        let src = r#"
(let sum {acc n}
  (if (= n 0)
    acc
    (sum (+ acc n) (- n 1))))
"#;
        let erl = crate::compile("test", src).unwrap();
        // Curried entry: sum/1
        assert!(erl.contains("sum(Acc) ->"), "missing curried entry:\n{erl}");
        // Multi-arg impl: sum/2
        assert!(
            erl.contains("sum(Acc, N) ->"),
            "missing multi-arg impl:\n{erl}"
        );
        // Self-recursive call uses sum/2 directly (not curried)
        assert!(erl.contains("sum("), "missing recursive call:\n{erl}");
        // Both arities exported
        assert!(erl.contains("sum/1"), "sum/1 not exported:\n{erl}");
        assert!(erl.contains("sum/2"), "sum/2 not exported:\n{erl}");
    }

    #[test]
    fn single_arg_function_unchanged() {
        let src = r#"
(let double {x}
  (* 2 x))
"#;
        let erl = crate::compile("test", src).unwrap();
        // Only one function, no /2
        assert!(erl.contains("double(X) ->"), "missing function:\n{erl}");
        assert!(!erl.contains("double/2"), "unexpected double/2:\n{erl}");
    }

    #[test]
    fn zero_arg_function_emits_arity_zero_wrapper() {
        let src = r#"
(let stop {} 1)
"#;
        let erl = crate::compile("test", src).unwrap();
        assert!(
            erl.contains("stop(_Unit) ->"),
            "missing Unit-arg entry for zero-arg function:\n{erl}"
        );
        assert!(
            erl.contains("stop() ->\n    stop(unit)."),
            "missing arity-zero wrapper for qualified zero-arg calls:\n{erl}"
        );
    }

    #[test]
    fn partial_application_uses_curried_entry() {
        // (add 1) applied partially — must call add/1, not add/2
        let src = r#"
(let add {a b} (+ a b))
(let inc {x} (add 1 x))
"#;
        let erl = crate::compile("test", src).unwrap();
        // inc calls add with both args → LocalCallMulti → add(1, X)
        assert!(
            erl.contains("add(1, X)") || erl.contains("add(1,X)"),
            "expected add(1, X):\n{erl}"
        );
    }

    #[test]
    fn nested_curried_full_application_uses_multi_arg_call() {
        let src = r#"
(let sum {acc n}
  (if (= n 0)
    acc
    ((sum (+ acc n)) (- n 1))))
"#;
        let erl = crate::compile("test", src).unwrap();
        assert!(
            erl.contains("sum((Acc + N), (N - 1))") || erl.contains("sum((Acc + N),(N - 1))"),
            "expected direct multi-arg recursive call:\n{erl}"
        );
    }

    #[test]
    fn builtin_boolean_operator_can_appear_in_value_position() {
        let src = r#"
(let choose_or {} or)
"#;
        let erl = crate::compile("test", src).unwrap();
        assert!(
            erl.contains("fun(A__) -> fun(B__) -> (A__ orelse B__) end end"),
            "expected first-class builtin operator lowering:\n{erl}"
        );
    }

    #[test]
    fn int_modulo_lowers_to_erlang_rem() {
        let src = r#"
(let mod_two {x} (% x 2))
"#;
        let erl = crate::compile("test", src).unwrap();
        assert!(
            erl.contains(" rem "),
            "expected `%` to lower to Erlang rem:\n{erl}"
        );
    }

    #[test]
    fn or_pattern_match_expands_all_alternatives() {
        let src = r#"
(type Token [A B C D E F])
(let classify {token}
  (match token
    A | B | C ~> 1
    D | E | F ~> 0))
"#;
        let erl = crate::compile("test", src).unwrap();
        assert!(
            erl.contains("a -> 1"),
            "expected first alternative case arm:\n{erl}"
        );
        assert!(
            erl.contains("b -> 1"),
            "expected second alternative case arm:\n{erl}"
        );
        assert!(
            erl.contains("c -> 1"),
            "expected third alternative case arm:\n{erl}"
        );
        assert!(
            erl.contains("d -> 0"),
            "expected fourth alternative case arm:\n{erl}"
        );
        assert!(
            erl.contains("e -> 0"),
            "expected fifth alternative case arm:\n{erl}"
        );
        assert!(
            erl.contains("f -> 0"),
            "expected sixth alternative case arm:\n{erl}"
        );
    }

    #[test]
    fn multi_payload_constructor_application_lowers_to_tagged_tuple() {
        let src = r#"
(type IpAddress
  [(IpV4 ~ Int Int Int Int)
   (IpV6 ~ Int Int Int Int Int Int Int Int)])
(let main {} (IpV4 1 2 3 4))
"#;
        let erl = crate::compile("test", src).unwrap();
        assert!(
            erl.contains("{ipv4, 1, 2, 3, 4}") || erl.contains("{ipv4,1,2,3,4}"),
            "expected constructor call to lower to tagged tuple with four payload values:\n{erl}"
        );
    }

    #[test]
    fn multi_payload_constructor_value_position_lowers_to_curried_function() {
        let src = r#"
(type PairValue [(PairValue ~ Int Int)])
(let make {} PairValue)
"#;
        let erl = crate::compile("test", src).unwrap();
        assert!(
            erl.contains("fun(X0__) -> fun(X1__) -> {pairvalue, X0__, X1__} end end")
                || erl.contains("fun(X0__)->fun(X1__)->{pairvalue,X0__,X1__}end end"),
            "expected constructor value position to lower to curried constructor lambda:\n{erl}"
        );
    }

    #[test]
    fn extern_remote_call_quotes_reserved_band_function_name() {
        let src = r#"
(extern let bitwise_and ~ (Int -> Int -> Int) erlang/band)
"#;
        let erl = crate::compile("test", src).unwrap();
        assert!(
            erl.contains("erlang:'band'("),
            "expected reserved remote function atom to be quoted:\n{erl}"
        );
    }

    #[test]
    fn extern_remote_call_quotes_reserved_bsr_function_name() {
        let src = r#"
(extern let bitwise_shift_right ~ (Int -> Int -> Int) erlang/bsr)
"#;
        let erl = crate::compile("test", src).unwrap();
        assert!(
            erl.contains("erlang:'bsr'("),
            "expected reserved remote function atom to be quoted:\n{erl}"
        );
    }

    #[test]
    fn record_constructor_named_fields_follow_declaration_order() {
        let src = r#"
(type Point [(:x ~ Int) (:y ~ Int)])
(let main {} (:x (Point :y 2 :x 1)))
"#;
        let erl = crate::compile("test", src).unwrap();
        assert!(
            erl.contains("{point, 1, 2}") || erl.contains("{point,1,2}"),
            "expected tuple layout to follow record declaration order:\n{erl}"
        );
    }

    #[test]
    fn record_update_lowers_to_setelement() {
        let src = r#"
(type Point [(:x ~ Int) (:y ~ Int)])
(let main {} (:x (with (Point :x 1 :y 2) :x 10)))
"#;
        let erl = crate::compile("test", src).unwrap();
        assert!(
            erl.contains("erlang:setelement(2"),
            "expected record update lowering via setelement/3:\n{erl}"
        );
    }

    #[test]
    fn polymorphic_field_access_lowers_to_tag_dispatch() {
        let src = r#"
(type ContinuePayload [(:id ~ Int) (:selector ~ Int)])
(type Initialised [(:selector ~ Bool)])
(let read_selector {x} (:selector x))
(let main {} (read_selector (ContinuePayload :id 0 :selector 1)))
"#;
        let erl = crate::compile("test", src).unwrap();
        assert!(
            erl.contains("case erlang:element(1, Fld"),
            "expected tag dispatch for polymorphic field access:\n{erl}"
        );
        assert!(
            erl.contains("continuepayload -> erlang:element(3"),
            "expected ContinuePayload selector index in dispatch:\n{erl}"
        );
        assert!(
            erl.contains("initialised -> erlang:element(2"),
            "expected Initialised selector index in dispatch:\n{erl}"
        );
    }

    #[test]
    fn polymorphic_record_update_lowers_to_tag_dispatch() {
        let src = r#"
(type ContinuePayload [(:id ~ Int) (:selector ~ Int)])
(type Initialised [(:selector ~ Bool)])
(let set_selector {x v} (with x :selector v))
(let main {} (set_selector (ContinuePayload :id 0 :selector 1) 2))
"#;
        let erl = crate::compile("test", src).unwrap();
        assert!(
            erl.contains("case erlang:element(1, RUpdDyn"),
            "expected tag dispatch for polymorphic record update:\n{erl}"
        );
        assert!(
            erl.contains("continuepayload -> erlang:setelement(3"),
            "expected ContinuePayload selector update index in dispatch:\n{erl}"
        );
        assert!(
            erl.contains("initialised -> erlang:setelement(2"),
            "expected Initialised selector update index in dispatch:\n{erl}"
        );
    }

    #[test]
    fn let_shadowing_uses_distinct_erlang_bindings() {
        let src = r#"
(let main {}
  (let [x 1
        x (+ x 1)]
    x))
"#;
        let erl = crate::compile("test", src).unwrap();
        assert!(
            erl.contains("X__l0 = 1"),
            "expected first let binding to use a fresh local var:\n{erl}"
        );
        assert!(
            erl.contains("X__l1 = (X__l0 + 1)") || erl.contains("X__l1=(X__l0+1)"),
            "expected inner let to shadow via a different Erlang var:\n{erl}"
        );
    }

    #[test]
    fn single_arg_function_param_can_shadow_top_level_function_name() {
        let src = r#"
(type FileInfo [(:mode ~ Int)])
(let file_info {x} x)
(let read_mode {file_info} (:mode file_info))
"#;
        let erl = crate::compile("test", src).unwrap();
        assert!(
            erl.contains("erlang:element(2, File_info)"),
            "expected parameter `file_info` to lower as a local var:\n{erl}"
        );
        assert!(
            !erl.contains("fun file_info/1"),
            "parameter shadowing should not lower to top-level function reference:\n{erl}"
        );
    }

    #[test]
    fn multi_arg_function_param_can_shadow_top_level_function_name() {
        let src = r#"
(let file_info {x} x)
(let pick_left {file_info y} file_info)
"#;
        let erl = crate::compile("test", src).unwrap();
        assert!(
            erl.contains("pick_left(File_info, Y) ->") && erl.contains("File_info."),
            "expected multi-arg body to reference parameter binding:\n{erl}"
        );
        assert!(
            !erl.contains("pick_left(File_info, Y) -> fun file_info/1.")
                && !erl.contains("pick_left(File_info,Y) -> fun file_info/1."),
            "multi-arg parameter shadowing incorrectly lowered to function ref:\n{erl}"
        );
    }

    #[test]
    fn lambda_param_can_shadow_top_level_function_name() {
        let src = r#"
(let file_info {x} x)
(let main {} ((f {file_info} -> file_info) 1))
"#;
        let erl = crate::compile("test", src).unwrap();
        assert!(
            erl.contains("fun(File_info) -> File_info end"),
            "expected lambda body to reference parameter binding:\n{erl}"
        );
        assert!(
            !erl.contains("fun(File_info) -> fun file_info/1 end"),
            "lambda parameter shadowing incorrectly lowered to function ref:\n{erl}"
        );
    }

    #[test]
    fn emit_atom_quotes_non_identifier_atoms() {
        assert_eq!(emit_expr(&ir::Expr::Atom("ok".into())), "ok");
        assert_eq!(
            emit_expr(&ir::Expr::Atom("map/takeresult".into())),
            "'map/takeresult'"
        );
    }

    #[test]
    fn emit_pattern_quotes_non_identifier_atoms() {
        assert_eq!(emit_pattern(&ir::Pattern::Atom("none".into())), "none");
        assert_eq!(
            emit_pattern(&ir::Pattern::Atom("map/takeresult".into())),
            "'map/takeresult'"
        );
    }
}
