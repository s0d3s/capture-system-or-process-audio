use crate::enums::ALL_ENUM_SPECS;
use pyo3_introspection::{
    introspect_cdylib,
    model::{Argument, Arguments, Attribute, Class, Constant, Expr, Function, Module},
    module_stub_files,
};
use std::path::{Path, PathBuf};

fn mod_type(name: &str) -> Expr {
    Expr::Attribute {
        value: Box::new(Expr::Name {
            id: "capture_system_or_process_audio".into(),
        }),
        attr: name.into(),
    }
}

fn rewrite_expr_no_typing_extensions(expr: &mut Expr) {
    match expr {
        Expr::Attribute { value, attr } => {
            if let Expr::Name { id } = value.as_ref()
                && id == "typing_extensions"
                && attr == "Buffer"
            {
                *expr = Expr::Attribute {
                    value: Box::new(Expr::Name {
                        id: "typing".into(),
                    }),
                    attr: "Any".into(),
                };
                return;
            }
            rewrite_expr_no_typing_extensions(value);
        }
        Expr::BinOp { left, right, .. } => {
            rewrite_expr_no_typing_extensions(left);
            rewrite_expr_no_typing_extensions(right);
        }
        Expr::Tuple { elts } | Expr::List { elts } => {
            for e in elts {
                rewrite_expr_no_typing_extensions(e);
            }
        }
        Expr::Subscript { value, slice } => {
            rewrite_expr_no_typing_extensions(value);
            rewrite_expr_no_typing_extensions(slice);
        }
        Expr::Constant { .. } | Expr::Name { .. } => {}
    }
}

fn sanitize_function(func: &mut Function) {
    func.decorators.retain(|d| {
        !matches!(
            d,
            Expr::Attribute { value, attr }
                if matches!(value.as_ref(), Expr::Name { id } if id == "typing_extensions")
                    && attr == "disjoint_base"
        )
    });
    for arg in &mut func.arguments.positional_only_arguments {
        if let Some(ann) = &mut arg.annotation {
            rewrite_expr_no_typing_extensions(ann);
        }
    }
    for arg in &mut func.arguments.arguments {
        if let Some(ann) = &mut arg.annotation {
            rewrite_expr_no_typing_extensions(ann);
        }
    }
    for arg in &mut func.arguments.keyword_only_arguments {
        if let Some(ann) = &mut arg.annotation {
            rewrite_expr_no_typing_extensions(ann);
        }
    }
    if let Some(vararg) = &mut func.arguments.vararg
        && let Some(ann) = &mut vararg.annotation
    {
        rewrite_expr_no_typing_extensions(ann);
    }
    if let Some(kwarg) = &mut func.arguments.kwarg
        && let Some(ann) = &mut kwarg.annotation
    {
        rewrite_expr_no_typing_extensions(ann);
    }
    if let Some(ret) = &mut func.returns {
        rewrite_expr_no_typing_extensions(ret);
    }
}

pub fn normalize_introspected_module(mut module: Module) -> Module {
    module.incomplete = false;
    module.classes.retain(|c| !c.name.starts_with('_'));
    module.functions.retain(|f| !f.name.starts_with('_'));
    module.attributes.retain(|a| a.name != "__all__");

    if !module.attributes.iter().any(|a| a.name == "__version__") {
        module.attributes.push(Attribute {
            name: "__version__".into(),
            value: None,
            annotation: Some(Expr::Subscript {
                value: Box::new(Expr::Attribute {
                    value: Box::new(Expr::Name {
                        id: "typing".into(),
                    }),
                    attr: "Final".into(),
                }),
                slice: Box::new(Expr::Name { id: "str".into() }),
            }),
            docstring: None,
        });
    }
    module.attributes.sort_by(|a, b| a.name.cmp(&b.name));

    for cls in &mut module.classes {
        cls.decorators.retain(|d| {
            !matches!(
                d,
                Expr::Attribute { value, attr }
                    if matches!(value.as_ref(), Expr::Name { id } if id == "typing_extensions")
                        && attr == "disjoint_base"
            )
        });
        for m in &mut cls.methods {
            sanitize_function(m);
            if cls.name == "ManagedStream" {
                match m.name.as_str() {
                    "read_async" | "__anext__" => {
                        m.is_async = true;
                        m.returns = Some(mod_type("ReadOutcome"));
                    }
                    "__aenter__" => {
                        m.is_async = true;
                        m.returns = Some(mod_type("ManagedStream"));
                    }
                    "__aexit__" => {
                        m.is_async = true;
                        m.returns = Some(Expr::Name { id: "bool".into() });
                    }
                    _ => {}
                }
            }
            if cls.name == "CaptureRequest" && m.name == "request_permissions_async" {
                m.is_async = true;
                m.returns = Some(mod_type("PermissionReport"));
            }
        }
        if cls.name == "PcmBlock" && !cls.methods.iter().any(|m| m.name == "__buffer__") {
            cls.methods.push(Function {
                name: "__buffer__".into(),
                decorators: Vec::new(),
                arguments: Arguments {
                    positional_only_arguments: vec![
                        Argument {
                            name: "self".into(),
                            default_value: None,
                            annotation: None,
                        },
                        Argument {
                            name: "flags".into(),
                            default_value: None,
                            annotation: Some(Expr::Name { id: "int".into() }),
                        },
                    ],
                    arguments: Vec::new(),
                    vararg: None,
                    keyword_only_arguments: Vec::new(),
                    kwarg: None,
                },
                returns: Some(Expr::Name {
                    id: "memoryview".into(),
                }),
                is_async: false,
                docstring: None,
            });
            cls.methods.sort_by(|a, b| a.name.cmp(&b.name));
        }
    }

    for func in &mut module.functions {
        sanitize_function(func);
        if func.name == "request_permissions_async" {
            func.is_async = true;
            func.returns = Some(mod_type("PermissionReport"));
        }
    }

    for spec in ALL_ENUM_SPECS {
        module.classes.push(Class {
            name: spec.name.to_string(),
            bases: vec![Expr::Attribute {
                value: Box::new(Expr::Name { id: "enum".into() }),
                attr: "Enum".into(),
            }],
            methods: Vec::new(),
            attributes: spec
                .variants
                .iter()
                .map(|v| Attribute {
                    name: v.member.to_string(),
                    value: Some(Expr::Constant {
                        value: Constant::Str(v.value.to_string()),
                    }),
                    annotation: None,
                    docstring: None,
                })
                .collect(),
            decorators: Vec::new(),
            inner_classes: Vec::new(),
            docstring: None,
        });
    }
    module.classes.sort_by(|a, b| a.name.cmp(&b.name));
    module.functions.sort_by(|a, b| a.name.cmp(&b.name));
    module
}

pub fn assemble_pyi_from_cdylib(cdylib_path: &Path) -> Result<String, String> {
    let raw_module =
        introspect_cdylib(cdylib_path, "capture_system_or_process_audio").map_err(|e| {
            format!(
                "introspect_cdylib failed on {}: {e:#}",
                cdylib_path.display()
            )
        })?;
    let normalized = normalize_introspected_module(raw_module);
    let files = module_stub_files(&normalized);
    files
        .get(Path::new("__init__.pyi"))
        .cloned()
        .ok_or_else(|| "missing __init__.pyi in module_stub_files output".to_string())
}

pub fn find_built_cdylib() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let deps_dir = exe.parent()?;
    let profile_dir = deps_dir.parent()?;
    let candidates = [
        profile_dir.join("capture_system_or_process_audio.dll"),
        profile_dir.join("libcapture_system_or_process_audio.so"),
        profile_dir.join("libcapture_system_or_process_audio.dylib"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}
