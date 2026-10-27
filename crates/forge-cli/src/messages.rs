//! Every text the CLI shows to a person, keyed for i18n: pt-BR (the default) and en-US
//! (`FORGE_LANG=en`). Logs and identifiers stay in English; the messages the daemon puts in
//! its API errors are English too (the CLI localises by `code`, docs/09).
//!
//! `{0}`, `{1}`... are replaced by the arguments of [`format_message`]; both languages of a
//! key must use the same placeholders (checked by a test).

/// Language of the messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    PtBr,
    EnUs,
}

impl Lang {
    /// From the value of `FORGE_LANG` (`en`, `en-US`, `en_US`, `pt`, `pt-BR`...); anything
    /// else is pt-BR.
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(|v| v.trim().to_ascii_lowercase()) {
            Some(v) if v == "en" || v.starts_with("en-") || v.starts_with("en_") => Lang::EnUs,
            _ => Lang::PtBr,
        }
    }

    /// From the environment.
    pub fn from_env() -> Self {
        Self::parse(std::env::var("FORGE_LANG").ok().as_deref())
    }
}

/// Replaces `{0}`, `{1}`... in `template` with the arguments.
pub fn format_message(template: &str, args: &[&dyn std::fmt::Display]) -> String {
    let mut out = template.to_owned();
    for (i, arg) in args.iter().enumerate() {
        out = out.replace(&format!("{{{i}}}"), &arg.to_string());
    }
    out
}

macro_rules! messages {
    ($($key:ident => ($pt:literal, $en:literal),)*) => {
        /// A message key.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Msg {
            $($key),*
        }

        impl Msg {
            /// The text in `lang`.
            pub fn text(self, lang: Lang) -> &'static str {
                match self {
                    $(Msg::$key => match lang {
                        Lang::PtBr => $pt,
                        Lang::EnUs => $en,
                    }),*
                }
            }

            /// Every key (tests).
            pub const ALL: &'static [Msg] = &[$(Msg::$key),*];
        }
    };
}

messages! {
    // generic
    ErrorPrefix => ("erro:", "error:"),
    Yes => ("sim", "yes"),
    No => ("não", "no"),
    On => ("ligado", "on"),
    Off => ("desligado", "off"),
    Cancelled => ("Operação cancelada.", "Operation cancelled."),
    Cancelling => ("Cancelando...", "Cancelling..."),
    ConfirmPrompt => ("[s/N]", "[y/N]"),
    ConfirmNeedsYes => (
        "{0} Peça confirmação com --yes (sem terminal não é possível perguntar).",
        "{0} Confirm with --yes (there is no terminal to ask on)."
    ),
    NeedsElevation => (
        "{0} precisa de confirmação de administrador (UAC) e --no-elevate foi usado.",
        "{0} needs administrator confirmation (UAC) and --no-elevate was given."
    ),
    UacNotice => (
        "Precisa de confirmação de administrador (UAC)...",
        "Needs administrator confirmation (UAC)..."
    ),
    HintElevation => (
        "Rode de novo sem --no-elevate e aceite o prompt do Windows.",
        "Run it again without --no-elevate and accept the Windows prompt."
    ),
    HintUnauthorized => (
        "O token em state/ipc.token não confere; reinicie o daemon com `forge daemon restart`.",
        "The token in state/ipc.token does not match; restart the daemon with `forge daemon restart`."
    ),
    StartingDaemon => ("Iniciando o daemon ({0})...", "Starting the daemon ({0})..."),
    DaemonNotRunning => (
        "o daemon não está rodando na raiz {0} (e --no-daemon impede iniciá-lo)",
        "the daemon is not running on root {0} (and --no-daemon prevents starting it)"
    ),
    RemoteNotAvailable => (
        "O perfil remoto {0} não pode ser usado ainda: o modo remoto chega no roadmap M6-E3.",
        "Remote profile {0} cannot be used yet: remote mode arrives with roadmap M6-E3."
    ),
    RemoteProfileMissing => (
        "o perfil {0} não existe (veja `forge remote list`)",
        "profile {0} does not exist (see `forge remote list`)"
    ),
    McpNotAvailable => (
        "O servidor MCP (forge-mcp) ainda não está disponível nesta instalação.",
        "The MCP server (forge-mcp) is not available in this installation yet."
    ),
    // table headers
    HName => ("NOME", "NAME"),
    HState => ("ESTADO", "STATE"),
    HVersion => ("VERSÃO", "VERSION"),
    HVersions => ("VERSÕES", "VERSIONS"),
    HPorts => ("PORTAS", "PORTS"),
    HPort => ("PORTA", "PORT"),
    HUptime => ("ATIVO HÁ", "UPTIME"),
    HType => ("TIPO", "TYPE"),
    HServer => ("SERVIDOR", "SERVER"),
    HTarget => ("DESTINO", "TARGET"),
    HCategory => ("CATEGORIA", "CATEGORY"),
    HInstalled => ("INSTALADAS", "INSTALLED"),
    HDescription => ("DESCRIÇÃO", "DESCRIPTION"),
    HTool => ("FERRAMENTA", "TOOL"),
    HTools => ("FERRAMENTAS", "TOOLS"),
    HCurrent => ("ATUAL", "CURRENT"),
    HLatest => ("ÚLTIMA", "LATEST"),
    HPolicy => ("POLÍTICA", "POLICY"),
    HUpdate => ("ATUALIZAÇÃO", "UPDATE"),
    HUpdated => ("ATUALIZADO", "UPDATED"),
    HFrom => ("DE", "FROM"),
    HTo => ("PARA", "TO"),
    HAutomatic => ("AUTOMÁTICA", "AUTOMATIC"),
    HAvailable => ("DISPONÍVEIS", "AVAILABLE"),
    HVariants => ("VARIANTES", "VARIANTS"),
    HUsedBy => ("USADA POR", "USED BY"),
    HSize => ("TAMANHO", "SIZE"),
    HGlobal => ("Global", "Global"),
    HProject => ("Projeto", "Project"),
    HProjectFile => ("Arquivo do projeto", "Project file"),
    HExecutable => ("Executável", "Executable"),
    HRole => ("PAPEL", "ROLE"),
    HPath => ("CAMINHO", "PATH"),
    HOrigin => ("ORIGEM", "ORIGIN"),
    HSeries => ("SÉRIE", "SERIES"),
    HCompiler => ("COMPILADOR", "COMPILER"),
    HWorkers => ("WORKERS", "WORKERS"),
    HRequests => ("REQUISIÇÕES", "REQUESTS"),
    HProcess => ("PROCESSO", "PROCESS"),
    HCommand => ("COMANDO", "COMMAND"),
    HOwner => ("DONO", "OWNER"),
    HHolder => ("PROCESSO NA PORTA", "HOLDER"),
    HInstance => ("INSTÂNCIA", "INSTANCE"),
    HTables => ("TABELAS", "TABLES"),
    HStarted => ("INÍCIO", "STARTED"),
    HRoot => ("Raiz", "Root"),
    HMode => ("Modo", "Mode"),
    HJobs => ("Jobs ativos", "Active jobs"),
    HConnections => ("Conexões", "Connections"),
    HEnabled => ("Ativado", "Enabled"),
    HResolver => ("Resolvedor", "Resolver"),
    HResolverRunning => ("Resolvedor rodando", "Resolver running"),
    HResolved => ("Resolveu", "Resolved"),
    HExpected => ("Esperado", "Expected"),
    HVia => ("Via", "Via"),
    HSubject => ("ASSUNTO", "SUBJECT"),
    HExpires => ("VALIDADE", "EXPIRES"),
    HMessages => ("Mensagens", "Messages"),
    HUnread => ("Não lidas", "Unread"),
    HReceived => ("RECEBIDA", "RECEIVED"),
    HCode => ("Código", "Code"),
    HCaFingerprint => ("Impressão da CA", "CA fingerprint"),
    HLastSeen => ("ÚLTIMO USO", "LAST SEEN"),
    HOfficial => ("OFICIAL", "OFFICIAL"),
    HImported => ("IMPORTADO", "IMPORTED"),
    HNote => ("NOTA", "NOTE"),
    // empty results
    NoServices => ("Nenhum serviço instalado ainda. Use `forge tool install`.", "No services installed yet. Use `forge tool install`."),
    NoSites => ("Nenhum site. Use `forge site add <pasta>` ou `forge park <pasta>`.", "No sites. Use `forge site add <dir>` or `forge park <dir>`."),
    NoResults => ("Nada encontrado.", "Nothing found."),
    NoTools => ("Nenhuma ferramenta.", "No tools."),
    NoVersionsInstalled => ("Nenhuma versão instalada.", "No versions installed."),
    NoFiles => ("Nenhum arquivo.", "No files."),
    NoJobs => ("Nenhum job.", "No jobs."),
    NoPhp => ("Nenhum PHP instalado. Use `forge tool install php`.", "No PHP installed. Use `forge tool install php`."),
    NoPools => ("Nenhum pool de PHP.", "No PHP pools."),
    NoExtensions => ("Nenhuma extensão.", "No extensions."),
    NoPorts => ("Nenhuma porta reservada.", "No ports reserved."),
    NoParked => ("Nenhuma pasta registrada. Use `forge park <pasta>`.", "No parked folders. Use `forge park <dir>`."),
    NoProcesses => ("Nenhum processo.", "No processes."),
    NoDatabases => ("Nenhum banco de dados.", "No databases."),
    NoCertificates => ("Nenhum certificado.", "No certificates."),
    NoMail => ("Nenhuma mensagem.", "No messages."),
    NoStats => ("Sem métricas ainda.", "No metrics yet."),
    NoRegistries => ("Nenhum registry.", "No registries."),
    NoProfiles => ("Nenhum perfil.", "No profiles."),
    NoClients => ("Nenhum cliente pareado.", "No paired clients."),
    NoLogLines => ("(sem linhas)", "(no lines)"),
    NothingToDiscover => ("Nada novo em bin/.", "Nothing new in bin/."),
    NothingImported => ("Nada importado.", "Nothing imported."),
    AllUpToDate => ("Tudo atualizado.", "Everything is up to date."),
    // states
    StateRunning => ("rodando", "running"),
    StateStarting => ("iniciando", "starting"),
    StateStopping => ("parando", "stopping"),
    StateStopped => ("parado", "stopped"),
    StateCrashed => ("caiu", "crashed"),
    StateFailed => ("falhou", "failed"),
    StateMaintenance => ("manutenção", "maintenance"),
    JobRunning => ("rodando", "running"),
    JobDone => ("concluído", "done"),
    JobFailed => ("falhou", "failed"),
    JobCancelled => ("cancelado", "cancelled"),
    PoolWorkers => ("{0}/{1} workers", "{0}/{1} workers"),
    PortBound => ("em uso", "bound"),
    PortReserved => ("reservada", "reserved"),
    PortConflict => ("CONFLITO", "CONFLICT"),
    PortExternal => ("(externo)", "(external)"),
    KindStatic => ("estático", "static"),
    Portable => ("portátil", "portable"),
    Installed => ("instalado", "installed"),
    Managed => ("gerenciado", "managed"),
    Bundled => ("embutida", "bundled"),
    // daemon
    DaemonStopped => ("Daemon parado (raiz {0}).", "Daemon stopped (root {0})."),
    DaemonAlreadyRunning => ("O daemon já está rodando (PID {0}).", "The daemon is already running (PID {0})."),
    DaemonStarted => ("Daemon iniciado (PID {0}).", "Daemon started (PID {0})."),
    DaemonAlreadyStopped => ("O daemon já estava parado.", "The daemon was already stopped."),
    DaemonStoppedNow => ("Daemon parado.", "Daemon stopped."),
    DaemonRestarted => ("Daemon reiniciado (PID {0}).", "Daemon restarted (PID {0})."),
    DaemonStopTimeout => ("o daemon não parou em {0} s", "the daemon did not stop within {0} s"),
    DoctorOk => ("{0} ok", "{0} ok"),
    DoctorWarnings => ("{0} avisos", "{0} warnings"),
    DoctorFailures => ("{0} falhas", "{0} failures"),
    DoctorVersions => (
        "forge {0}, daemon {1}, API do daemon {2}, API do forge {3}",
        "forge {0}, daemon {1}, daemon API {2}, forge API {3}"
    ),
    // tools
    ToolInstalled => ("Instalado {0} {1}", "Installed {0} {1}"),
    ToolRemoved => ("Removido {0}", "Removed {0}"),
    ToolRolledBack => ("{0} voltou para {1}", "{0} rolled back to {1}"),
    ToolPolicySet => ("Política de {0}: {1}", "Policy of {0}: {1}"),
    ToolAdded => ("Registrado {0} {1}", "Registered {0} {1}"),
    ToolNowUses => ("{0} agora usa {1}", "{0} now uses {1}"),
    ToolUnknownFolder => ("pasta não identificada: {0}", "unidentified folder: {0}"),
    ConfirmRemoveTool => ("Remover {0}?", "Remove {0}?"),
    UpdateNeedsTarget => (
        "Informe uma ferramenta ou --all (ou use --check para só listar).",
        "Give a tool or --all (or use --check to just list)."
    ),
    // services
    InstanceCreated => ("Instância criada: {0}/{1}", "Instance created: {0}/{1}"),
    InstanceRemoved => ("Instância removida: {0}", "Instance removed: {0}"),
    ConfirmRemoveInstance => ("Remover a instância {0}?", "Remove instance {0}?"),
    ConfirmRemoveInstanceData => (
        "Remover a instância {0} E APAGAR OS DADOS?",
        "Remove instance {0} AND DELETE ITS DATA?"
    ),
    LogsLagged => ("{0} linhas perdidas (consumo lento)", "{0} lines lost (slow consumer)"),
    XdebugSet => ("Xdebug do PHP {0}: {1}", "Xdebug of PHP {0}: {1}"),
    NotOpenedNoTerminal => (
        "Sem terminal: o arquivo não foi aberto no editor.",
        "No terminal: the file was not opened in the editor."
    ),
    // sites
    SiteCreated => ("Site criado: {0}", "Site created: {0}"),
    SiteRemoved => ("Site removido: {0}", "Site removed: {0}"),
    SiteUpdated => ("Site atualizado: {0}", "Site updated: {0}"),
    SiteSecured => ("HTTPS ligado em {0}", "HTTPS enabled on {0}"),
    SiteUnsecured => ("HTTPS desligado em {0}", "HTTPS disabled on {0}"),
    SiteCaseSet => ("{0}: case-sensitive = {1}", "{0}: case-sensitive = {1}"),
    ConfirmRemoveSite => (
        "Remover o site {0}? (os arquivos do projeto não são apagados)",
        "Remove site {0}? (project files are not deleted)"
    ),
    NoSiteForDir => ("nenhum site usa a pasta {0}", "no site uses the folder {0}"),
    Parked => ("Pasta registrada: {0} ({1} sites)", "Folder parked: {0} ({1} sites)"),
    Unparked => ("Pasta removida.", "Folder unparked."),
    ProjectDown => ("Projeto parado.", "Project stopped."),
    ShareStarting => ("Túnel iniciando...", "Tunnel starting..."),
    ShareStopped => ("Túnel encerrado.", "Tunnel stopped."),
    SharedAt => ("Publicado em {0}", "Shared at {0}"),
    LintFixed => ("corrigido", "fixed"),
    LintSummary => (
        "{0} problemas ({1} arquivos verificados, {2} referências)",
        "{0} issues ({1} files scanned, {2} references)"
    ),
    // case sensitivity
    CaseUnsupported => ("sem suporte", "unsupported"),
    CaseOn => ("case-sensitive", "case-sensitive"),
    CaseOff => ("case-insensitive", "case-insensitive"),
    CaseUnknown => ("desconhecido", "unknown"),
    CaseAlready => ("{0} já estava no estado pedido.", "{0} was already in the requested state."),
    CaseConverted => (
        "{0} convertida: {1} arquivos, {2} pastas, {3}",
        "{0} converted: {1} files, {2} folders, {3}"
    ),
    // databases
    DatabaseCreated => ("Banco criado: {0}", "Database created: {0}"),
    DatabaseDropped => ("Banco apagado: {0}", "Database dropped: {0}"),
    DatabaseExported => ("Exportado para {0} ({1})", "Exported to {0} ({1})"),
    DatabaseImported => ("Importado em {0}", "Imported into {0}"),
    ConfirmDropDatabase => ("APAGAR o banco {0}?", "DROP database {0}?"),
    // net / tls / mail
    DnsApplied => ("aplicada", "applied"),
    DnsHostsFallback => ("arquivo hosts (alternativa)", "hosts file (fallback)"),
    DnsNotApplied => ("não aplicada", "not applied"),
    DnsResolves => ("resolve", "resolves"),
    DnsDoesNotResolve => ("NÃO resolve como esperado", "does NOT resolve as expected"),
    CaMissing => (
        "Ainda não há CA local. Use `forge tls ca install`.",
        "There is no local CA yet. Use `forge tls ca install`."
    ),
    ConfirmRotateCa => (
        "Criar uma CA nova e reemitir todos os certificados?",
        "Create a new CA and re-issue every certificate?"
    ),
    FileWritten => ("Arquivo gravado: {0}", "File written: {0}"),
    BinaryNeedsOutput => (
        "{0} é binário: use -o <arquivo> para gravá-lo.",
        "{0} is binary: use -o <file> to save it."
    ),
    LanUnexposed => ("Acesso pela rede local desligado.", "LAN access turned off."),
    ConfirmClearMail => ("Apagar todas as mensagens?", "Delete all messages?"),
    MailCleared => ("Mensagens apagadas.", "Messages deleted."),
    // shell
    ExecNeedsCommand => ("Faltou o comando depois de `--`.", "Missing the command after `--`."),
    ExecFailed => ("não foi possível executar {0}: {1}", "could not run {0}: {1}"),
    PathForgeboxEntries => ("Entradas do Forgebox no PATH do usuário:", "Forgebox entries in the user PATH:"),
    PathNone => ("(nenhuma)", "(none)"),
    PathMissing => ("faltando; use `forge path add`", "missing; use `forge path add`"),
    // jobs / stats
    InvalidJobId => ("job_id inválido: {0}", "invalid job_id: {0}"),
    JobCancelRequested => ("Cancelamento pedido.", "Cancellation requested."),
    JobNotRunning => ("O job não estava rodando.", "The job was not running."),
    JobFinished => ("Job concluído.", "Job finished."),
    ExportRangeInvalid => ("--from precisa ser anterior a --to.", "--from must be earlier than --to."),
    // remote
    RemoteSaved => ("Perfil {0} guardado.", "Profile {0} saved."),
    RemoteSelected => ("Perfil padrão: {0}", "Default profile: {0}"),
    RemoteRemoved => ("Perfil {0} removido.", "Profile {0} removed."),
    RemotePairingLater => (
        "O pareamento (--code) chega com o modo remoto (M6-E3); o código não foi guardado.",
        "Pairing (--code) arrives with remote mode (M6-E3); the code was not stored."
    ),
    RemoteListening => ("Aceitando conexões remotas.", "Accepting remote connections."),
    InvalidClientId => ("id de cliente inválido: {0}", "invalid client id: {0}"),
    ConfirmRevoke => ("Revogar o cliente {0}?", "Revoke client {0}?"),
    ClientRevoked => ("Cliente revogado.", "Client revoked."),
    // registry / system
    RegistryAdded => ("Registry adicionado: {0}", "Registry added: {0}"),
    RegistryRemoved => ("Registry removido: {0}", "Registry removed: {0}"),
    ArtifactWritten => ("{0} ({1}, sha256 {2})", "{0} ({1}, sha256 {2})"),
    BackupScheduled => ("Backup agendado: {0} {1}", "Backup scheduled: {0} {1}"),
    SelfUpdated => ("Forgebox atualizado.", "Forgebox updated."),
    RestartRequired => ("Reinicie o Forgebox para usar a nova versão.", "Restart Forgebox to use the new version."),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placeholders(text: &str) -> Vec<String> {
        let mut found: Vec<String> = text
            .match_indices('{')
            .filter_map(|(at, _)| {
                let rest = &text[at + 1..];
                let end = rest.find('}')?;
                rest[..end]
                    .chars()
                    .all(|c| c.is_ascii_digit())
                    .then(|| rest[..end].to_owned())
            })
            .collect();
        found.sort();
        found.dedup();
        found
    }

    #[test]
    fn both_languages_have_the_same_placeholders() {
        for msg in Msg::ALL {
            assert_eq!(
                placeholders(msg.text(Lang::PtBr)),
                placeholders(msg.text(Lang::EnUs)),
                "{msg:?}"
            );
        }
    }

    #[test]
    fn no_message_is_empty_or_untranslated() {
        for msg in Msg::ALL {
            let (pt, en) = (msg.text(Lang::PtBr), msg.text(Lang::EnUs));
            assert!(!pt.is_empty() && !en.is_empty(), "{msg:?}");
        }
    }

    #[test]
    fn language_comes_from_forge_lang() {
        assert_eq!(Lang::parse(None), Lang::PtBr);
        assert_eq!(Lang::parse(Some("pt-BR")), Lang::PtBr);
        assert_eq!(Lang::parse(Some("en")), Lang::EnUs);
        assert_eq!(Lang::parse(Some("EN_us")), Lang::EnUs);
        assert_eq!(Lang::parse(Some("fr")), Lang::PtBr);
    }

    #[test]
    fn placeholders_are_replaced() {
        assert_eq!(
            format_message("{1} depois de {0}", &[&"a", &2]),
            "2 depois de a"
        );
        assert_eq!(Msg::PoolWorkers.text(Lang::PtBr), "{0}/{1} workers");
    }
}
