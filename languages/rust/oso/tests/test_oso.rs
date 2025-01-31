use oso::{Oso, PolarClass};
use std::path::{Path, PathBuf};

mod common;

fn test_file_path() -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"));
    path.join(Path::new("tests/test_oso.polar"))
}

#[derive(PolarClass, Debug, Clone, PartialEq)]
struct Actor {
    #[polar(attribute)]
    name: String,
}

impl Actor {
    pub fn new(name: String) -> Self {
        Self { name }
    }

    pub fn companies(&self) -> Vec<Company> {
        vec![Company { id: 1 }]
    }
}

#[derive(PolarClass, Debug, Clone, PartialEq)]
struct Widget {
    #[polar(attribute)]
    id: i64,
}

impl Widget {
    pub fn new(id: i64) -> Self {
        Self { id }
    }
}

#[derive(PolarClass, Debug, Clone, PartialEq)]
struct Company {
    #[polar(attribute)]
    id: i64,
}

impl Company {
    pub fn new(id: i64) -> Self {
        Self { id }
    }

    pub fn role(&self, actor: Actor) -> String {
        if actor.name == "president" {
            "admin".to_string()
        } else {
            "guest".to_string()
        }
    }
}

fn test_oso() -> Oso {
    let mut oso = Oso::new();
    oso.register_class(
        Actor::get_polar_class_builder()
            .set_constructor(Actor::new)
            .add_method("companies", Actor::companies)
            .build(),
    )
    .unwrap();
    oso.register_class(Widget::get_polar_class()).unwrap();
    oso.register_class(
        Company::get_polar_class_builder()
            .set_constructor(Company::new)
            .add_method("role", Company::role)
            .with_equality_check()
            .build(),
    )
    .unwrap();

    let path = test_file_path();
    oso.load_file(path).unwrap();

    oso
}

#[test]
fn test_is_allowed() -> oso::Result<()> {
    common::setup();
    let oso = test_oso();

    let actor = Actor::new(String::from("guest"));
    let resource = Widget::new(1);
    let action = "get";

    assert!(oso.is_allowed(actor, action, resource)?);

    let actor = Actor::new(String::from("president"));
    let resource = Company::new(1);
    let action = "create";

    assert!(oso.is_allowed(actor, action, resource)?);

    Ok(())
}

#[test]
fn test_query_rule() -> oso::Result<()> {
    common::setup();
    let oso = test_oso();

    let actor = Actor::new(String::from("guest"));
    let resource = Widget::new(1);
    let action = "get";
    let mut query = oso.query_rule("allow", (actor, action, resource))?;

    assert!(query.next().is_some());

    Ok(())
}

#[test]
fn test_fail() -> oso::Result<()> {
    common::setup();
    let oso = test_oso();

    let actor = Actor::new(String::from("guest"));
    let resource = Widget::new(1);
    let action = "not_allowed";

    assert!(!oso.is_allowed(actor, action, resource)?);

    Ok(())
}

#[test]
fn test_instance_from_external_call() -> oso::Result<()> {
    common::setup();
    let oso = test_oso();

    let actor = Actor::new(String::from("guest"));
    let resource = Company::new(1);

    assert!(oso.is_allowed(actor, "frob", resource)?);

    Ok(())
}

#[test]
#[ignore = "PartialEq is not yet implemented for `oso::host::Class`"]
fn test_allow_model() -> oso::Result<()> {
    common::setup();
    let oso = test_oso();

    let actor = Actor::new(String::from("auditor"));
    assert!(oso.is_allowed(actor, "list", Company::get_polar_class())?);

    let actor = Actor::new(String::from("auditor"));
    assert!(!oso.is_allowed(actor, "list", Widget::get_polar_class())?);

    Ok(())
}
