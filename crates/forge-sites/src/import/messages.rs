//! User-facing texts of the Laragon import (pt-BR) with their i18n keys. Until the i18n
//! catalogs exist (see `crate::messages`), they live here.

type Msg = (&'static str, String);

pub fn not_laragon(path: &str) -> Msg {
    (
        "import.not_laragon",
        format!(
            "'{path}' não parece uma instalação do Laragon (esperado: usr/laragon.ini, bin/ ou www/)."
        ),
    )
}

pub fn tool_unconfirmed(version: &str) -> Msg {
    (
        "import.tool_unconfirmed",
        format!(
            "a versão {version} vem só do nome da pasta (o executável não respondeu); não foi registrada"
        ),
    )
}

pub fn tool_no_version() -> Msg {
    (
        "import.tool_no_version",
        "não foi possível descobrir a versão; não foi registrada".to_owned(),
    )
}

pub fn tool_link(target: &str) -> Msg {
    (
        "import.tool_link",
        format!(
            "link (junction) para '{target}', a versão ativa do Laragon; a pasta real é importada"
        ),
    )
}

pub fn bin_unknown() -> Msg {
    (
        "import.bin_unknown",
        "nenhuma versão reconhecida pelos manifestos do registry (outra ferramenta, layout diferente ou versão não identificável)"
            .to_owned(),
    )
}

pub fn vhost_not_auto() -> Msg {
    (
        "import.vhost_not_auto",
        "arquivo do Apache que não é auto.*.conf (configuração manual); crie o site com `forge site add` ou `forge proxy add`"
            .to_owned(),
    )
}

pub fn vhost_no_site(why: &str) -> Msg {
    (
        "import.vhost_no_site",
        format!("nenhum site neste arquivo: {why}"),
    )
}

pub fn vhost_unreadable(why: &str) -> Msg {
    (
        "import.vhost_unreadable",
        format!("arquivo não pôde ser lido: {why}"),
    )
}

pub fn root_missing(root: &str) -> Msg {
    (
        "import.root_missing",
        format!("a pasta do site não existe: {root}"),
    )
}

pub fn host_renamed(from: &str, to: &str) -> Msg {
    (
        "import.host_renamed",
        format!("o host '{from}' virou '{to}' (hosts só aceitam letras minúsculas, números e '-')"),
    )
}

pub fn host_unusable(host: &str) -> Msg {
    (
        "import.host_unusable",
        format!("o nome '{host}' não é um host válido"),
    )
}

pub fn wildcards_dropped() -> Msg {
    (
        "import.wildcards_dropped",
        "aliases curinga (*.nome) não são importados".to_owned(),
    )
}

pub fn duplicate_vhost(host: &str, other: &str) -> Msg {
    (
        "import.duplicate_vhost",
        format!("'{host}' já veio de {other}"),
    )
}

pub fn data_unknown_tool() -> Msg {
    (
        "import.data_unknown_tool",
        "nenhum serviço com data dir do registry tem este nome (esperado algo como mysql-8)"
            .to_owned(),
    )
}

pub fn data_no_version(tool: &str, hint: &str) -> Msg {
    (
        "import.data_no_version",
        format!("nenhuma versão importada de '{tool}' serve para '{hint}'"),
    )
}

pub fn data_empty() -> Msg {
    ("import.data_empty", "pasta vazia".to_owned())
}

pub fn docroot_missing() -> Msg {
    (
        "import.docroot_missing",
        "a pasta de projetos do Laragon não existe".to_owned(),
    )
}

// ------------------------------------------------------------------- apply

pub fn already_registered() -> Msg {
    (
        "import.already_registered",
        "já registrada (importação anterior ou descoberta automática)".to_owned(),
    )
}

pub fn copied_to(dest: &str) -> Msg {
    ("import.copied_to", format!("copiada para {dest}"))
}

pub fn copy_dest_exists(dest: &str) -> Msg {
    (
        "import.copy_dest_exists",
        format!("o destino {dest} já existe e não está registrado; nada foi sobrescrito"),
    )
}

pub fn copy_failed(why: &str) -> Msg {
    ("import.copy_failed", format!("a cópia falhou: {why}"))
}

pub fn referenced() -> Msg {
    (
        "import.referenced",
        "usada onde está (referência), sem copiar nem renomear".to_owned(),
    )
}

pub fn instance_exists() -> Msg {
    (
        "import.instance_exists",
        "já existe uma instância com este nome".to_owned(),
    )
}

pub fn instance_tool_missing(folder: &str) -> Msg {
    (
        "import.instance_tool_missing",
        format!("a versão '{folder}' não foi registrada; a instância não pode ser criada"),
    )
}

pub fn data_referenced(version: &str) -> Msg {
    (
        "import.data_referenced",
        format!(
            "instância sobre a pasta do Laragon (referência) com a versão {version}; para mover para o data/ do Forgebox use --move-data (o servidor do Laragon precisa estar parado)"
        ),
    )
}

pub fn data_moved(dest: &str) -> Msg {
    ("import.data_moved", format!("pasta movida para {dest}"))
}

pub fn data_move_failed(why: &str) -> Msg {
    (
        "import.data_move_failed",
        format!("não foi possível mover ({why}); mantida como referência"),
    )
}

pub fn data_move_dest_exists(dest: &str) -> Msg {
    (
        "import.data_move_dest_exists",
        format!("o destino {dest} já existe; mantida como referência"),
    )
}

pub fn site_exists() -> Msg {
    (
        "import.site_exists",
        "já existe um site com este host".to_owned(),
    )
}

pub fn site_tld(hosts: &[String], tlds: &[String]) -> Msg {
    (
        "import.site_tld",
        format!(
            "nenhum dos hosts ({}) termina em um TLD configurado ({}); ajuste com `forge config set tld`",
            hosts.join(", "),
            tlds.iter()
                .map(|t| format!(".{t}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
}

pub fn alias_taken(alias: &str) -> Msg {
    (
        "import.alias_taken",
        format!("alias '{alias}' não importado: já pertence a outro site"),
    )
}

pub fn alias_invalid(alias: &str) -> Msg {
    (
        "import.alias_invalid",
        format!("alias '{alias}' não importado: nome inválido"),
    )
}

pub fn site_failed(why: &str) -> Msg {
    (
        "import.site_failed",
        format!("o site não pôde ser criado: {why}"),
    )
}

pub fn pref_tld_same(tld: &str) -> Msg {
    ("import.pref_tld_same", format!("o Forgebox já usa .{tld}"))
}

pub fn pref_tld_differs(laragon: &str, current: &str) -> Msg {
    (
        "import.pref_tld_differs",
        format!(
            "o Laragon usa .{laragon} e o Forgebox .{current}; mantido (mude com `forge config set tld {laragon}` se quiser)"
        ),
    )
}

pub fn parked_added() -> Msg {
    (
        "import.parked_added",
        "pasta de projetos adicionada; cada subpasta vira um site .test".to_owned(),
    )
}

pub fn parked_present() -> Msg {
    (
        "import.parked_present",
        "já estava na lista de pastas de projetos".to_owned(),
    )
}

pub fn parked_offer_conversion(dir: &str) -> Msg {
    (
        "import.parked_offer_conversion",
        format!(
            "pasta com conteúdo continua case-insensitive; para paridade com Linux use --convert-case ou `forge fs case-sensitive convert {dir}`"
        ),
    )
}

pub fn case_converted() -> Msg {
    (
        "import.case_converted",
        "convertida para case-sensitive (reversível com --revert)".to_owned(),
    )
}

pub fn case_failed(why: &str) -> Msg {
    (
        "import.case_failed",
        format!("a conversão para case-sensitive falhou: {why}"),
    )
}
