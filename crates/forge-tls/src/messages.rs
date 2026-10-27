//! User-facing texts (pt-BR) of `forge-tls`, kept together until the i18n layer exists.
//! Keys are the stable identifiers the GUI/CLI will translate.

/// `tls.trust.node`: Node.js does not read the Windows store.
pub const TRUST_NODE_TITLE: &str = "Node.js";
pub const TRUST_NODE_STEPS: &[&str] = &[
    "O Node.js não usa o repositório de certificados do Windows.",
    "Os processos gerenciados pelo Forgebox já recebem NODE_EXTRA_CA_CERTS apontando para a CA local.",
    "Para terminais fora do Forgebox, defina a variável de ambiente do usuário NODE_EXTRA_CA_CERTS com o caminho abaixo e abra um terminal novo.",
];

/// `tls.trust.firefox`.
pub const TRUST_FIREFOX_TITLE: &str = "Firefox";
pub const TRUST_FIREFOX_STEPS: &[&str] = &[
    "O Firefox 120 ou mais novo já confia nas CAs instaladas pelo usuário no Windows (security.enterprise_roots.enabled).",
    "Em versões antigas, ou se a opção estiver desligada, crie o arquivo distribution\\policies.json na pasta de instalação do Firefox com o conteúdo abaixo e reinicie o navegador.",
    "Alternativa sem arquivo: Configurações > Privacidade e Segurança > Certificados > Ver certificados > Autoridades > Importar a CA exportada.",
];

/// `tls.trust.python`.
pub const TRUST_PYTHON_TITLE: &str = "Python (requests, pip)";
pub const TRUST_PYTHON_STEPS: &[&str] = &[
    "requests e pip usam o pacote certifi, não o Windows.",
    "Defina REQUESTS_CA_BUNDLE (requests) ou PIP_CERT (pip) com o caminho da CA abaixo, ou use o pacote truststore (Python 3.10+), que lê o repositório do Windows.",
];

/// `tls.trust.java`.
pub const TRUST_JAVA_TITLE: &str = "Java";
pub const TRUST_JAVA_STEPS: &[&str] = &[
    "A JVM tem o próprio cacerts. Importe a CA exportada com o keytool abaixo (ou use -Djavax.net.ssl.trustStoreType=Windows-ROOT no Java 11+).",
];

/// Portable mode: the CA is never added to the Windows store (docs/08 §2.2).
pub const PORTABLE_NO_TRUST: &str = "Modo portátil: a CA local não é instalada no repositório do Windows. Exporte a CA e importe-a manualmente onde precisar.";

/// The CA expires within the warning window (docs/06 §6.1: 90 days).
pub const CA_EXPIRES_SOON: &str = "A CA local expira em menos de 90 dias. Gire a CA (forge tls ca rotate) para emitir novos certificados para todos os sites.";

/// A configured TLD is not covered by the CA name constraints.
pub const CA_MISSING_TLDS: &str = "A CA local não cobre todos os TLDs configurados. Gire a CA (forge tls ca rotate) para incluí-los.";
