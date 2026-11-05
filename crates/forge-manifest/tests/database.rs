//! `spec.database` / `spec.mail` of the real registry (docs/04 §1.8, §1.9): the blocks
//! validate and render the exact SQL and argv the daemon will run.

use std::path::PathBuf;

use forge_manifest::database::{DbQuery, DbVars, RenderError, RenderedCommand};
use forge_manifest::schema::Manifest;
use forge_manifest::schema::tool::{DatabaseProtocol, DatabaseSpec, MailApi, MailSpec};
use forge_manifest::{load_file, validate};

fn tool(id: &str) -> forge_manifest::schema::tool::ToolManifest {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../registry/tools")
        .join(format!("{id}.yaml"));
    let manifest = load_file(&path).unwrap_or_else(|e| panic!("{id}: {e}"));
    let issues = validate(&manifest);
    assert!(issues.is_empty(), "{id}: {issues:#?}");
    match manifest {
        Manifest::Tool(tool) => tool,
        other => panic!("{id}: {:?}", other.kind()),
    }
}

fn database(id: &str) -> DatabaseSpec {
    tool(id)
        .spec
        .database
        .unwrap_or_else(|| panic!("{id}: no spec.database"))
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn vars(port: u16) -> DbVars {
    DbVars::new().database("loja").port("main", port)
}

#[test]
fn mysql_create_renders_exact_sql_and_argv() {
    let db = database("mysql");
    assert_eq!(db.protocol, DatabaseProtocol::Mysql);
    assert_eq!(
        db.render_sql(DbQuery::Create, &vars(3306)).unwrap(),
        "CREATE DATABASE `loja` CHARACTER SET utf8mb4"
    );
    assert_eq!(
        db.render_query(DbQuery::Create, &vars(3306)).unwrap(),
        RenderedCommand {
            exe: "bin/mysql.exe".to_owned(),
            args: strings(&[
                "--protocol=tcp",
                "--host=127.0.0.1",
                "--port=3306",
                "--user=root",
                "--batch",
                "--skip-column-names",
                "--execute=CREATE DATABASE `loja` CHARACTER SET utf8mb4",
            ]),
        }
    );
}

#[test]
fn postgresql_create_renders_exact_sql_and_argv() {
    let db = database("postgresql");
    assert_eq!(db.protocol, DatabaseProtocol::Postgresql);
    assert_eq!(
        db.render_query(DbQuery::Create, &vars(5432)).unwrap(),
        RenderedCommand {
            exe: "bin/psql.exe".to_owned(),
            args: strings(&[
                "-X",
                "-h",
                "127.0.0.1",
                "-p",
                "5432",
                "-U",
                "postgres",
                "-d",
                "postgres",
                "-v",
                "ON_ERROR_STOP=1",
                "-At",
                "-F",
                "\t",
                "-c",
                "CREATE DATABASE \"loja\" ENCODING 'UTF8' TEMPLATE template0",
            ]),
        }
    );
}

#[test]
fn list_and_drop_render_for_every_family() {
    for (id, drop) in [
        ("mysql", "DROP DATABASE `loja`"),
        ("mariadb", "DROP DATABASE `loja`"),
        ("postgresql", "DROP DATABASE \"loja\""),
    ] {
        let db = database(id);
        assert_eq!(
            db.render_sql(DbQuery::Drop, &vars(1)).unwrap(),
            drop,
            "{id}"
        );
        let list = db
            .render_query(DbQuery::List, &DbVars::new().port("main", 1))
            .unwrap();
        assert!(list.args.last().unwrap().contains("SELECT"), "{id}");
    }
    assert!(
        database("mysql")
            .render_sql(DbQuery::List, &DbVars::new())
            .unwrap()
            .contains("information_schema.schemata")
    );
}

#[test]
fn mariadb_uses_its_own_clients() {
    let db = database("mariadb");
    assert_eq!(db.client.exe, "bin/mariadb.exe");
    assert_eq!(db.dump.as_ref().unwrap().exe, "bin/mariadb-dump.exe");
    assert!(
        db.system_databases
            .contains(&"information_schema".to_owned())
    );
}

#[test]
fn dump_restore_shell_url_and_env_render() {
    let db = database("mysql");
    let dump = db
        .render_command(db.dump.as_ref().unwrap(), &vars(3307))
        .unwrap();
    assert_eq!(dump.exe, "bin/mysqldump.exe");
    assert_eq!(
        dump.args.first().map(String::as_str),
        Some("--protocol=tcp")
    );
    assert_eq!(dump.args.last().map(String::as_str), Some("loja"));
    assert!(dump.args.contains(&"--port=3307".to_owned()));
    let restore = db
        .render_command(db.restore.as_ref().unwrap(), &vars(3307))
        .unwrap();
    assert_eq!(restore.args.last().map(String::as_str), Some("loja"));
    let shell = db
        .render_command(db.shell.as_ref().unwrap(), &vars(3307))
        .unwrap();
    assert_eq!(shell.exe, "bin/mysql.exe");
    assert_eq!(
        db.render_url(&vars(3307)).unwrap(),
        "mysql://root@127.0.0.1:3307/loja"
    );
    let env = db.render_env(&vars(3307)).unwrap();
    assert_eq!(env["DB_DATABASE"], "loja");
    assert_eq!(env["DB_PORT"], "3307");
    assert_eq!(env["DB_CONNECTION"], "mysql");

    let pg = database("postgresql");
    assert_eq!(
        pg.render_url(&vars(5433)).unwrap(),
        "postgresql://postgres@127.0.0.1:5433/loja"
    );
    assert_eq!(
        pg.render_env(&vars(5433)).unwrap()["DB_CONNECTION"],
        "pgsql"
    );
}

#[test]
fn invalid_names_are_refused_before_any_substitution() {
    let db = database("mysql");
    for name in ["lo`ja", "loja; DROP", "a b", "", &"x".repeat(65)] {
        let vars = DbVars::new().database(name).port("main", 3306);
        assert_eq!(
            db.render_query(DbQuery::Create, &vars),
            Err(RenderError::InvalidDatabase(name.to_owned())),
            "{name:?}"
        );
    }
    let vars = vars(3306).charset("utf8'; --");
    assert!(matches!(
        db.render_sql(DbQuery::Create, &vars),
        Err(RenderError::InvalidCharset(_))
    ));
    // A charset given by the caller replaces the default.
    assert_eq!(
        db.render_sql(DbQuery::Create, &vars_with_charset())
            .unwrap(),
        "CREATE DATABASE `loja` CHARACTER SET latin1"
    );
    // Missing values are an error, never an empty string.
    assert_eq!(
        db.render_query(DbQuery::Create, &DbVars::new()),
        Err(RenderError::MissingValue("database".to_owned()))
    );
}

fn vars_with_charset() -> DbVars {
    vars(3306).charset("latin1")
}

#[test]
fn mailpit_declares_the_mail_block() {
    let tool = tool("mailpit");
    let mail: MailSpec = tool.spec.mail.expect("spec.mail");
    assert_eq!(mail.api, MailApi::MailpitV1);
    assert_eq!(
        (mail.http_port.as_str(), mail.smtp_port.as_str()),
        ("http", "smtp")
    );
    assert_eq!(mail.env["MAIL_MAILER"], "smtp");
    assert!(tool.spec.database.is_none());
}

#[test]
fn only_database_tools_declare_the_block() {
    for id in ["mysql", "mariadb", "postgresql"] {
        assert!(tool(id).spec.database.is_some(), "{id}");
    }
    for id in ["nginx", "php", "mailpit"] {
        assert!(tool(id).spec.database.is_none(), "{id}");
    }
}
