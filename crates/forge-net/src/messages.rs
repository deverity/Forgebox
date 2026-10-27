//! User-facing texts of the DNS side (pt-BR) until the i18n infrastructure exists. The
//! keys are the stable codes ([`DnsHintCode`], [`PolicyState`]); the UI translates by code.

use crate::dns::diagnose::DnsHintCode;
use crate::policy::PolicyState;

/// Explanation of a diagnosis hint; `detail` is the process, adapter, rule or address
/// the hint is about.
pub fn hint(code: DnsHintCode, detail: &str) -> String {
    match code {
        DnsHintCode::NotInZone => format!(
            "{detail} não pertence a nenhuma zona do Forgebox; use um nome terminado no TLD configurado (ex.: loja.test)."
        ),
        DnsHintCode::ServerNotRunning => {
            "O resolvedor DNS do Forgebox não está rodando. Inicie o daemon ou rode `forge dns enable`.".to_owned()
        }
        DnsHintCode::ServerNotOnPort53 => format!(
            "O resolvedor escuta em {detail}, fora da porta 53: a regra NRPT não pode apontar para ele e só o fallback via hosts funciona."
        ),
        DnsHintCode::ServerNotAnswering => format!(
            "O resolvedor do Forgebox não respondeu como esperado: {detail}."
        ),
        DnsHintCode::NrptMissing => format!(
            "Não há regra NRPT para {detail}. Rode `forge dns enable` (pede confirmação de administrador uma vez)."
        ),
        DnsHintCode::NrptPointsElsewhere => format!(
            "A regra NRPT de {detail}, não para o resolvedor do Forgebox. Rode `forge dns enable` para corrigi-la."
        ),
        DnsHintCode::GroupPolicyNrpt => format!(
            "Há regras NRPT de política de grupo ({detail}); enquanto existirem, o Windows ignora as regras locais. Comum com VPN corporativa ou DirectAccess."
        ),
        DnsHintCode::HostsOverride => format!(
            "O arquivo hosts mapeia este nome para {detail} fora do bloco do Forgebox; essa linha vence o DNS."
        ),
        DnsHintCode::Port53Acrylic => format!(
            "O Acrylic DNS Proxy ({detail}) ocupa a porta 53. Desinstale-o ou desative-o: o Forgebox já resolve *.test sozinho."
        ),
        DnsHintCode::Port53Docker => format!(
            "O Docker ({detail}) escuta na porta 53 e pode responder por .test; o Forgebox usa outro endereço de loopback se precisar."
        ),
        DnsHintCode::Port53DnsServer => format!(
            "Outro servidor DNS ({detail}) usa a porta 53 no loopback."
        ),
        DnsHintCode::Port53Vpn => format!(
            "O cliente VPN ({detail}) usa a porta 53 e costuma sobrescrever o DNS da máquina."
        ),
        DnsHintCode::Port53SystemService => format!(
            "Um serviço do Windows ({detail}, em geral o Host Network Service do Hyper-V/WSL ou o Compartilhamento de Conexão) usa a porta 53; normalmente não atrapalha."
        ),
        DnsHintCode::Port53Other => format!("{detail} usa a porta 53."),
        DnsHintCode::VpnAdapter => format!(
            "Adaptador de VPN ativo ({detail}): VPNs podem sobrescrever o DNS ou instalar regras NRPT próprias."
        ),
        DnsHintCode::SystemMismatch => format!(
            "O Windows resolve este nome para {detail}, não para o endereço esperado."
        ),
        DnsHintCode::SystemUnresolved => format!(
            "O Windows não resolve este nome ({detail})."
        ),
    }
}

/// Label of a policy state.
pub fn policy_state(state: PolicyState) -> &'static str {
    match state {
        PolicyState::Applied => "regra NRPT ativa",
        PolicyState::HostsFallback => "fallback no arquivo hosts",
        PolicyState::NotApplied => "sem roteamento DNS",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_a_text() {
        for code in DnsHintCode::ALL {
            let text = hint(*code, "x");
            assert!(!text.trim().is_empty(), "{code:?}");
        }
        for state in [
            PolicyState::Applied,
            PolicyState::HostsFallback,
            PolicyState::NotApplied,
        ] {
            assert!(!policy_state(state).is_empty());
        }
    }
}
