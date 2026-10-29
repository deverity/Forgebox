//! The command line of `forge` (docs/09): every group and command, with the global
//! flags. Parsing only; the behaviour is in [`crate::commands`].

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use forge_ipc::base::ids::ToolId;

/// `forge`: thin client of the Forgebox daemon.
#[derive(Debug, Parser)]
#[command(
    name = "forge",
    version,
    about = "Forgebox: ambiente de desenvolvimento local (CLI)",
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,
    #[command(subcommand)]
    pub command: Command,
}

/// Flags accepted by every command (docs/09).
#[derive(Debug, Clone, Args)]
pub struct GlobalArgs {
    /// Saída em JSON estável (um documento por comando; JSON lines nos streams).
    #[arg(long, global = true)]
    pub json: bool,
    /// Só imprime o resultado (sem progresso nem mensagens de status).
    #[arg(short, long, global = true)]
    pub quiet: bool,
    /// Responde "sim" às confirmações (obrigatório sem terminal).
    #[arg(short, long, global = true)]
    pub yes: bool,
    /// Raiz da instalação (padrão: FORGEBOX_ROOT, portable.flag, root.toml, C:\Forgebox).
    #[arg(long, global = true, value_name = "DIR")]
    pub root: Option<PathBuf>,
    /// Perfil de conexão (`forge remote list`); o padrão é o daemon local.
    #[arg(long, global = true, value_name = "NOME")]
    pub context: Option<String>,
    /// Não inicia o daemon automaticamente.
    #[arg(long, global = true)]
    pub no_daemon: bool,
    /// Não dispara o prompt de elevação (UAC): ações que precisam falham com código 5.
    #[arg(long, global = true)]
    pub no_elevate: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Daemon: status, start, stop, restart, logs, doctor.
    #[command(subcommand)]
    Daemon(DaemonCmd),
    /// Diagnóstico completo: PATH, DNS, CA, portas, vcredist, Defender, versões.
    Doctor,
    /// Busca no(s) registry(ies).
    Search(SearchArgs),
    /// Catálogo de ferramentas: instalar, remover, atualizar.
    #[command(subcommand)]
    Tool(ToolCmd),
    /// Troca a versão ativa (junction `current`) de uma ferramenta.
    Use(UseArgs),
    /// Mostra versão/caminho ativo (global e do projeto atual).
    Which(WhichArgs),
    /// Inicia serviços (`all`, `nginx`, `mysql/default`, `php@8.3`).
    Start(TargetsArgs),
    /// Para serviços.
    Stop(TargetsArgs),
    /// Reinicia serviços.
    Restart(TargetsArgs),
    /// Recarrega a configuração de um serviço.
    Reload(ReloadArgs),
    /// Estado dos serviços.
    Status(StatusArgs),
    /// Instâncias de serviço: criar, remover, configurar, portas.
    #[command(subcommand)]
    Service(ServiceCmd),
    /// Logs de um alvo (nginx, php@8.3, mysql/default, site:loja.test, app:loja.test/web).
    Logs(LogsArgs),
    /// Versões e pools de PHP.
    #[command(subcommand)]
    Php(PhpCmd),
    /// Sites.
    #[command(subcommand)]
    Site(SiteCmd),
    /// Registra uma pasta cujas subpastas viram sites.
    Park(ParkArgs),
    /// Remove uma pasta registrada.
    Unpark(DirArg),
    /// Lista as pastas registradas.
    Parked,
    /// Cria um site para a pasta (equivalente valet).
    Link(LinkArgs),
    /// Remove o site da pasta.
    Unlink(OptDirArg),
    /// Proxies reversos.
    #[command(subcommand)]
    Proxy(ProxyCmd),
    /// Lê forgebox.yaml, prepara tudo e sobe os processes.
    Up(OptDirArg),
    /// Para o projeto.
    Down(OptDirArg),
    /// Processes de um site.
    Ps(PsArgs),
    /// Controla um process de um site.
    Run(RunArgs),
    /// Publica um site por um túnel.
    Share(ShareArgs),
    /// Cria um projeto a partir de uma stack.
    New(NewArgs),
    /// Bancos de dados.
    #[command(subcommand)]
    Db(DbCmd),
    /// DNS local.
    #[command(subcommand)]
    Dns(DnsCmd),
    /// Certificados e CA local.
    #[command(subcommand)]
    Tls(TlsCmd),
    /// Acesso pela rede local.
    #[command(subcommand)]
    Lan(LanCmd),
    /// Caixa de e-mail local (Mailpit).
    #[command(subcommand)]
    Mail(MailCmd),
    /// Abre um shell com o PATH do Forgebox.
    Shell(OptDirArg),
    /// Imprime a configuração de ambiente (`Invoke-Expression (forge env --powershell)`).
    Env(EnvArgs),
    /// Executa um comando com versões específicas, sem trocar `current`.
    Exec(ExecArgs),
    /// Entradas do PATH do usuário.
    #[command(subcommand)]
    Path(PathCmd),
    /// Sistema de arquivos: paridade com Linux (case-sensitive).
    #[command(subcommand)]
    Fs(FsCmd),
    /// Operações longas em curso.
    Jobs(JobsArgs),
    /// Métricas por site.
    Stats(StatsArgs),
    /// Processos do Forgebox por CPU/RAM.
    Top,
    /// Perfis de conexão e modo remoto.
    #[command(subcommand)]
    Remote(RemoteCmd),
    /// Registries.
    #[command(subcommand)]
    Registry(RegistryCmd),
    /// Importa de outra ferramenta.
    #[command(subcommand)]
    Import(ImportCmd),
    /// Bundles offline.
    #[command(subcommand)]
    Bundle(BundleCmd),
    /// Backups de bancos.
    #[command(subcommand)]
    Backup(BackupCmd),
    /// Configuração (forgebox.toml).
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Atualiza o Forgebox.
    SelfUpdate(SelfUpdateArgs),
    /// Gera o script de autocompletar do shell.
    Completions(CompletionsArgs),
    /// Servidor MCP para IDEs e agentes.
    #[command(subcommand)]
    Mcp(McpCmd),
}

// --- daemon -------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum DaemonCmd {
    /// Estado do daemon (pid, versão, uptime).
    Status,
    /// Inicia o daemon (forgeboxd) e espera ficar pronto.
    Start,
    /// Para o daemon (e a stack).
    Stop,
    /// Reinicia o daemon.
    Restart,
    /// Logs do daemon.
    Logs(DaemonLogsArgs),
    /// Diagnóstico completo (igual a `forge doctor`).
    Doctor,
}

#[derive(Debug, Args)]
pub struct DaemonLogsArgs {
    /// Acompanha novas linhas.
    #[arg(short, long)]
    pub follow: bool,
    /// Número de linhas finais.
    #[arg(short = 'n', long, value_name = "N")]
    pub lines: Option<u32>,
}

// --- tools --------------------------------------------------------------------------

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Texto da busca (nome, tag, descrição).
    #[arg(default_value = "")]
    pub text: String,
    /// Só esta categoria (`db`, `web`...).
    #[arg(long)]
    pub category: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum ToolCmd {
    /// Lista ferramentas.
    List {
        /// Só as instaladas.
        #[arg(long)]
        installed: bool,
        /// Só as com atualização.
        #[arg(long)]
        updates: bool,
    },
    /// Versões, variantes, política e sites que usam.
    Info {
        #[arg(value_parser = parse_tool_id)]
        tool: ToolId,
    },
    /// Instala (`php@8.3` = última patch da série, `php@8.3.36` = exata).
    Install {
        #[arg(required = true, value_name = "TOOL[@VERSAO]")]
        specs: Vec<String>,
        /// Variante da build (`ts`, `nts`).
        #[arg(long)]
        variant: Option<String>,
    },
    /// Remove uma versão.
    Remove {
        #[arg(value_name = "TOOL@VERSAO")]
        spec: String,
        /// Mantém a configuração em etc/.
        #[arg(long)]
        keep_config: bool,
    },
    /// Atualiza respeitando as políticas.
    Update {
        /// Uma ferramenta.
        #[arg(value_parser = parse_tool_id, conflicts_with = "all")]
        tool: Option<ToolId>,
        /// Todas.
        #[arg(long)]
        all: bool,
        /// Só lista o que há.
        #[arg(long)]
        check: bool,
    },
    /// Volta para a versão retida da série.
    Rollback {
        #[arg(value_name = "TOOL@SERIE")]
        spec: String,
    },
    /// Política de atualização.
    Policy {
        #[arg(value_parser = parse_tool_id)]
        tool: ToolId,
        policy: PolicyArg,
    },
    /// Registra um zip/pasta local como versão.
    AddLocal {
        #[arg(value_name = "ZIP|DIR")]
        source: PathBuf,
    },
    /// Varre bin/ por pastas não registradas.
    Discover,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum PolicyArg {
    Manual,
    Notify,
    AutoPatch,
    AutoMinor,
    Auto,
}

#[derive(Debug, Args)]
pub struct UseArgs {
    #[arg(value_parser = parse_tool_id)]
    pub tool: ToolId,
    /// Série ou versão exata (`8.4`, `22`, `3.13.1`).
    pub version: String,
}

#[derive(Debug, Args)]
pub struct WhichArgs {
    #[arg(value_parser = parse_tool_id)]
    pub tool: ToolId,
}

// --- services -----------------------------------------------------------------------

#[derive(Debug, Args)]
pub struct TargetsArgs {
    /// `all`, `nginx`, `mysql/default`, `php@8.3` (vazio = all).
    pub targets: Vec<String>,
}

#[derive(Debug, Args)]
pub struct ReloadArgs {
    pub target: String,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Atualiza a tabela a cada mudança de estado.
    #[arg(long)]
    pub watch: bool,
}

#[derive(Debug, Subcommand)]
pub enum ServiceCmd {
    /// Nova instância (`mysql@8.4 legacy --port 3307`).
    Create {
        #[arg(value_name = "TOOL[@VERSAO]")]
        tool: String,
        name: String,
        #[arg(long)]
        port: Option<u16>,
        /// Inicia junto com o daemon.
        #[arg(long)]
        autostart: bool,
    },
    /// Remove uma instância.
    Remove {
        #[arg(value_name = "TOOL/NOME")]
        instance: String,
        /// Apaga também os dados.
        #[arg(long)]
        delete_data: bool,
    },
    /// Abre os arquivos de configuração no editor.
    Config { target: String },
    /// Ajustes rápidos (`memory_limit=1G`).
    Set {
        target: String,
        #[arg(required = true, value_name = "CHAVE=VALOR")]
        settings: Vec<String>,
    },
    /// Mapa de portas e donos (inclui conflitos externos).
    Ports,
    /// Encerra o processo de fora do Forgebox que ocupa uma porta (pede confirmação).
    KillPort {
        port: u16,
        /// PID confirmado; sem ele, o dono atual da porta (`net.port_check`).
        #[arg(long)]
        pid: Option<u32>,
    },
}

#[derive(Debug, Args)]
pub struct LogsArgs {
    /// `nginx`, `nginx.error`, `php@8.3`, `mysql/default`, `site:loja.test`, `app:loja.test/web`.
    #[arg(required_unless_present = "list")]
    pub target: Option<String>,
    /// Lista os alvos de log disponíveis.
    #[arg(long, conflicts_with_all = ["target", "follow", "since", "grep", "lines", "before"])]
    pub list: bool,
    /// Acompanha novas linhas.
    #[arg(short, long)]
    pub follow: bool,
    /// Só linhas dos últimos N (`30s`, `10m`, `2h`, `1d`).
    #[arg(long, value_name = "DURACAO")]
    pub since: Option<String>,
    /// Só linhas que contêm o texto.
    #[arg(long)]
    pub grep: Option<String>,
    /// Número máximo de linhas.
    #[arg(short = 'n', long, value_name = "N")]
    pub lines: Option<u32>,
    /// Só linhas anteriores a este `seq` (páginas para trás).
    #[arg(long, value_name = "SEQ", conflicts_with = "follow")]
    pub before: Option<u64>,
}

// --- php ----------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum PhpCmd {
    /// Versões de PHP instaladas.
    List,
    /// Pools FastCGI.
    #[command(subcommand)]
    Pool(PhpPoolCmd),
    /// Extensões.
    #[command(subcommand)]
    Ext(PhpExtCmd),
    /// Xdebug por versão.
    Xdebug {
        version: String,
        mode: XdebugArg,
        #[arg(long)]
        port: Option<u16>,
    },
    /// Caminho do php.ini (ou abre no editor).
    Ini {
        version: String,
        /// Abre no editor.
        #[arg(long)]
        open: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum PhpPoolCmd {
    /// Estado dos pools.
    Status { version: Option<String> },
}

#[derive(Debug, Subcommand)]
pub enum PhpExtCmd {
    List { version: String },
    Enable { version: String, extension: String },
    Disable { version: String, extension: String },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum XdebugArg {
    On,
    Off,
    Trigger,
}

// --- sites --------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum SiteCmd {
    /// Lista os sites.
    List,
    /// Cria um site para uma pasta.
    Add {
        dir: PathBuf,
        #[arg(long)]
        host: Option<String>,
        /// Série (`8.3`) ou versão exata do PHP.
        #[arg(long)]
        php: Option<String>,
        /// Servidor web (`nginx`, `apache`, `caddy`).
        #[arg(long, value_parser = parse_tool_id)]
        server: Option<ToolId>,
        /// HTTPS com certificado local.
        #[arg(long)]
        secure: bool,
        #[arg(long)]
        driver: Option<String>,
    },
    /// Remove o site (arquivos do projeto nunca são apagados).
    Remove { site: String },
    /// Troca a versão de PHP ao vivo.
    Php { site: String, version: String },
    /// Liga HTTPS.
    Secure { site: String },
    /// Desliga HTTPS.
    Unsecure { site: String },
    /// Troca o servidor web.
    Server {
        site: String,
        #[arg(value_parser = parse_tool_id)]
        server: ToolId,
    },
    /// Aliases de host.
    Alias {
        site: String,
        action: AliasAction,
        alias: String,
    },
    /// Abre no navegador, editor, terminal ou explorer.
    Open {
        site: String,
        #[arg(long, conflicts_with_all = ["terminal", "explorer"])]
        editor: bool,
        #[arg(long, conflicts_with = "explorer")]
        terminal: bool,
        #[arg(long)]
        explorer: bool,
    },
    /// Diagnóstico: DNS, certificado, pool, root, driver.
    Info { site: String },
    /// Variáveis do `.env` do site (segredos mascarados).
    Env {
        site: String,
        /// Mostra também os valores secretos (só admin).
        #[arg(long)]
        reveal: bool,
    },
    /// Liga/desliga o modo case-sensitive do site.
    CaseSensitive { site: String, state: OnOff },
    /// Case Mismatch Finder (docs/15 §6).
    LintCase {
        site: String,
        /// Aplica as correções.
        #[arg(long)]
        fix: bool,
        /// Mostra o que seria corrigido sem alterar nada.
        #[arg(long, requires = "fix")]
        dry_run: bool,
        #[arg(long, value_enum, default_value = "auto")]
        strategy: StrategyArg,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum AliasAction {
    Add,
    Remove,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OnOff {
    On,
    Off,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum StrategyArg {
    Auto,
    Rename,
    Edit,
}

#[derive(Debug, Args)]
pub struct ParkArgs {
    pub dir: PathBuf,
    /// TLD dos sites desta pasta (padrão: o configurado).
    #[arg(long)]
    pub tld: Option<String>,
}

#[derive(Debug, Args)]
pub struct DirArg {
    pub dir: PathBuf,
}

#[derive(Debug, Args)]
pub struct OptDirArg {
    /// Pasta (padrão: a atual).
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct LinkArgs {
    /// Pasta (padrão: a atual).
    pub dir: Option<PathBuf>,
    #[arg(long)]
    pub host: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum ProxyCmd {
    /// Cria um proxy `host -> upstream`.
    Add {
        host: String,
        upstream: String,
        /// Encaminha WebSocket.
        #[arg(long)]
        ws: bool,
        #[arg(long)]
        secure: bool,
    },
    /// Remove um proxy.
    Remove { host: String },
}

#[derive(Debug, Args)]
pub struct PsArgs {
    pub site: Option<String>,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    pub site: String,
    pub process: String,
    pub action: RunAction,
    /// Com `logs`: acompanha novas linhas.
    #[arg(short, long)]
    pub follow: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RunAction {
    Start,
    Stop,
    Restart,
    Logs,
}

#[derive(Debug, Args)]
pub struct ShareArgs {
    pub site: String,
    /// `cloudflared`, `ngrok`, `expose`.
    #[arg(long, value_parser = parse_tool_id)]
    pub provider: Option<ToolId>,
    /// Encerra o túnel.
    #[arg(long)]
    pub stop: bool,
}

#[derive(Debug, Args)]
pub struct NewArgs {
    /// Stack (`laravel`, ...).
    pub stack: String,
    pub name: String,
    #[arg(long)]
    pub php: Option<String>,
    #[arg(long, value_parser = parse_tool_id)]
    pub db: Option<ToolId>,
    /// Pasta onde criar (padrão: www/).
    #[arg(long)]
    pub dir: Option<PathBuf>,
}

// --- db -----------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum DbCmd {
    List {
        instance: Option<String>,
    },
    Create {
        name: String,
        #[arg(long = "on", value_name = "INSTANCIA")]
        on: Option<String>,
        #[arg(long)]
        charset: Option<String>,
    },
    Drop {
        name: String,
        #[arg(long = "on", value_name = "INSTANCIA")]
        on: Option<String>,
    },
    Export {
        name: String,
        #[arg(long = "on", value_name = "INSTANCIA")]
        on: Option<String>,
        /// Arquivo de saída (`loja.sql.zst`).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    Import {
        name: String,
        file: PathBuf,
        #[arg(long = "on", value_name = "INSTANCIA")]
        on: Option<String>,
    },
    /// Abre o cliente CLI do banco.
    Shell {
        instance: String,
        database: Option<String>,
    },
    /// Imprime DSN/URL e variáveis para o .env.
    Url {
        instance: String,
        database: String,
    },
    /// Abre uma ferramenta gráfica (heidisql, dbeaver, adminer, phpmyadmin).
    Open {
        instance: String,
        #[arg(long = "with", value_parser = parse_tool_id)]
        with: ToolId,
        database: Option<String>,
    },
}

// --- net ----------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum DnsCmd {
    Status,
    Enable,
    Disable,
    /// Resolve um host como o Windows resolveria.
    Test {
        host: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum TlsCmd {
    /// CA local.
    #[command(subcommand)]
    Ca(TlsCaCmd),
    /// Emite o certificado de um host.
    Issue { host: String },
    /// Renova certificados.
    Renew {
        host: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Exporta o certificado de um host.
    Export {
        host: String,
        /// Formato PFX em vez de PEM.
        #[arg(long)]
        pfx: bool,
        #[arg(long)]
        password: Option<String>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum TlsCaCmd {
    Status,
    /// Instala a CA no store de confiança.
    Install {
        /// `user` (CurrentUser\Root, sem admin) ou `machine` (LocalMachine\Root, via helper).
        #[arg(long, value_enum, default_value = "user")]
        scope: ScopeArg,
    },
    /// Remove a CA do store de confiança.
    Uninstall {
        /// `user` (CurrentUser\Root) ou `machine` (LocalMachine\Root, via helper).
        #[arg(long, value_enum, default_value = "user")]
        scope: ScopeArg,
    },
    /// Exporta o certificado da CA.
    Export {
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Cria uma CA nova e reemite os certificados.
    Rotate,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ScopeArg {
    User,
    Machine,
}

#[derive(Debug, Subcommand)]
pub enum LanCmd {
    Expose { site: String },
    Unexpose { site: String },
    Qr { site: String },
}

// --- mail ---------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum MailCmd {
    Status,
    Open,
    List {
        #[arg(long)]
        unread: bool,
        /// Busca (`from:`, `subject:`, texto livre).
        #[arg(long, value_name = "TEXTO")]
        search: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        /// Pula as N mensagens mais novas (paginação).
        #[arg(long, value_name = "N")]
        offset: Option<u32>,
    },
    /// Mostra uma mensagem (e a marca como lida) ou baixa um anexo.
    Show {
        id: String,
        /// Não marca como lida.
        #[arg(long, conflicts_with = "attachment")]
        keep_unread: bool,
        /// Parte do anexo (`mail show <id> --json` lista as partes).
        #[arg(long, value_name = "PARTE")]
        attachment: Option<String>,
        /// Arquivo de saída do anexo.
        #[arg(short, long, requires = "attachment")]
        output: Option<PathBuf>,
    },
    /// Marca mensagens como lidas (ou não lidas).
    Read {
        #[arg(required = true, value_name = "ID")]
        ids: Vec<String>,
        /// Marca como não lidas.
        #[arg(long)]
        unread: bool,
    },
    /// Apaga mensagens (pede confirmação).
    Delete {
        #[arg(required = true, value_name = "ID")]
        ids: Vec<String>,
    },
    Clear,
    /// Imprime MAIL_* para o .env.
    Env,
}

// --- shell / env --------------------------------------------------------------------

#[derive(Debug, Args)]
pub struct EnvArgs {
    pub dir: Option<PathBuf>,
    #[arg(long, group = "format")]
    pub powershell: bool,
    #[arg(long, group = "format")]
    pub cmd: bool,
    #[arg(long, group = "format")]
    pub bash: bool,
}

#[derive(Debug, Args)]
pub struct ExecArgs {
    /// Versão de PHP só para este comando.
    #[arg(long, value_name = "VERSAO")]
    pub php: Option<String>,
    /// Comando e argumentos (depois de `--`).
    #[arg(required = true, allow_hyphen_values = true, last = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum PathCmd {
    Add,
    Remove,
    Show,
}

// --- fs -----------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum FsCmd {
    /// Paridade com Linux: pastas case-sensitive (NTFS).
    #[command(subcommand)]
    CaseSensitive(FsCaseCmd),
}

#[derive(Debug, Subcommand)]
pub enum FsCaseCmd {
    /// Estado de uma pasta (padrão: a atual).
    Status { dir: Option<PathBuf> },
    /// Liga a flag numa pasta vazia.
    Enable { dir: PathBuf },
    /// Desliga a flag numa pasta vazia.
    Disable { dir: PathBuf },
    /// Converte uma árvore existente.
    Convert {
        dir: PathBuf,
        /// Volta para case-insensitive.
        #[arg(long)]
        revert: bool,
    },
}

// --- jobs / stats -------------------------------------------------------------------

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct JobsArgs {
    /// Só os que estão rodando.
    #[arg(long)]
    pub active: bool,
    #[command(subcommand)]
    pub command: Option<JobsCmd>,
}

#[derive(Debug, Subcommand)]
pub enum JobsCmd {
    /// Acompanha um job até o fim.
    Watch { job_id: String },
    /// Pede o cancelamento.
    Cancel { job_id: String },
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct StatsArgs {
    /// Site (todos quando omitido).
    pub site: Option<String>,
    #[arg(long, value_enum, default_value = "5m")]
    pub window: WindowArg,
    /// Atualiza continuamente.
    #[arg(long)]
    pub watch: bool,
    #[command(subcommand)]
    pub command: Option<StatsCmd>,
}

#[derive(Debug, Subcommand)]
pub enum StatsCmd {
    /// Exporta amostras de um site.
    Export {
        site: String,
        /// Início (RFC 3339 ou relativo: `24h`, `7d`).
        #[arg(long)]
        from: String,
        /// Fim (padrão: agora).
        #[arg(long)]
        to: Option<String>,
        /// Formato.
        #[arg(short, long, value_enum, default_value = "csv")]
        output: ExportFormatArg,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum WindowArg {
    #[value(name = "1m")]
    OneMinute,
    #[value(name = "5m")]
    FiveMinutes,
    #[value(name = "1h")]
    OneHour,
    #[value(name = "24h")]
    OneDay,
    #[value(name = "7d")]
    SevenDays,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ExportFormatArg {
    Csv,
    Json,
}

// --- remote -------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum RemoteCmd {
    /// Guarda um perfil de conexão (wss://host:porta ou ssh://usuario@host).
    Add {
        name: String,
        url: String,
        /// Código de pareamento exibido pelo lado remoto.
        #[arg(long)]
        code: Option<String>,
    },
    List,
    /// Define o perfil padrão (`local` volta ao daemon local).
    Use {
        name: String,
    },
    Remove {
        name: String,
    },
    /// No lado remoto: gera um código de pareamento.
    Pair,
    /// No lado remoto: aceita conexões.
    Listen {
        #[arg(long, value_name = "ENDERECO:PORTA")]
        addr: String,
    },
    /// No lado remoto: clientes pareados.
    Clients,
    /// No lado remoto: revoga um cliente.
    Revoke {
        id: String,
    },
}

// --- registry / system --------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum RegistryCmd {
    List,
    Add {
        url: String,
        #[arg(long)]
        name: Option<String>,
    },
    Remove {
        name: String,
    },
    Update,
}

#[derive(Debug, Subcommand)]
pub enum ImportCmd {
    /// Importa uma instalação do Laragon (a pasta do Laragon só é lida).
    Laragon {
        path: PathBuf,
        /// Mostra o que seria importado, sem gravar nada.
        #[arg(long)]
        dry_run: bool,
        /// Copia as ferramentas para bin/<tool>/ em vez de usá-las onde estão.
        #[arg(long)]
        copy_tools: bool,
        /// Move os data dirs para data/<tool>/<nome> (mesmo volume) em vez de referenciá-los.
        #[arg(long)]
        move_data: bool,
        /// Converte as pastas de projetos para case-sensitive depois de importar.
        #[arg(long)]
        convert_case: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum BundleCmd {
    Export {
        #[arg(required = true, value_name = "TOOL[@VERSAO]")]
        specs: Vec<String>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    Import {
        file: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum BackupCmd {
    /// Backup agora (todas as instâncias ou uma).
    Now { instance: Option<String> },
    /// Agenda (`daily 02:00`, `weekly 03:30`, `off`).
    Schedule {
        frequency: FrequencyArg,
        /// Horário local `HH:MM`.
        at: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum FrequencyArg {
    Off,
    Daily,
    Weekly,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// Toda a configuração (valores efetivos; `--json` traz padrões e o JSON Schema).
    List,
    Get {
        key: String,
    },
    Set {
        key: String,
        /// Valor (JSON ou texto).
        value: String,
    },
}

#[derive(Debug, Args)]
pub struct SelfUpdateArgs {
    /// Só verifica.
    #[arg(long)]
    pub check: bool,
    #[arg(long)]
    pub channel: Option<String>,
}

#[derive(Debug, Args)]
pub struct CompletionsArgs {
    #[arg(value_enum)]
    pub shell: clap_complete::Shell,
}

#[derive(Debug, Subcommand)]
pub enum McpCmd {
    /// Servidor MCP via stdio.
    Serve,
}

/// `clap` value parser of tool ids (`php`, `mysql`).
fn parse_tool_id(value: &str) -> Result<ToolId, String> {
    value
        .parse::<ToolId>()
        .map_err(|e| format!("identificador de ferramenta inválido: {e}"))
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn the_command_tree_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn global_flags_work_after_the_command() {
        let cli =
            Cli::try_parse_from(["forge", "site", "list", "--json", "--root", r"D:\fb"]).unwrap();
        assert!(cli.global.json);
        assert_eq!(cli.global.root.unwrap(), PathBuf::from(r"D:\fb"));
    }

    #[test]
    fn exec_takes_everything_after_the_double_dash() {
        let cli =
            Cli::try_parse_from(["forge", "exec", "--php", "8.2", "--", "php", "-v", "--ini"])
                .unwrap();
        match cli.command {
            Command::Exec(args) => {
                assert_eq!(args.php.as_deref(), Some("8.2"));
                assert_eq!(args.command, ["php", "-v", "--ini"]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn jobs_without_a_subcommand_lists() {
        let cli = Cli::try_parse_from(["forge", "jobs", "--active"]).unwrap();
        match cli.command {
            Command::Jobs(args) => assert!(args.active && args.command.is_none()),
            other => panic!("{other:?}"),
        }
        assert!(Cli::try_parse_from(["forge", "jobs", "watch", "01ABC"]).is_ok());
    }

    #[test]
    fn invalid_tool_ids_are_usage_errors() {
        let err = Cli::try_parse_from(["forge", "tool", "info", "PHP!"]).unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }
}
