# ADR-0024 — Execução das operações do `forgebox-helper`: APIs sem shell, cópia verificada do instalador, CA restrita, log antes de agir

**Status:** aceito · **Data:** 2026-10-27 · Complementa a [ADR-0022](0022-elevated-helper-protocol.md) (protocolo).

## Contexto
A ADR-0022 fixou **o que** o helper aceita (allowlist `HelperOp`, validação no daemon e no helper) e **como** pedido e resposta atravessam o UAC (arquivos em `state/locks/`). Faltava decidir, para o binário `forgebox-helper.exe` (M1-E5-T4):

1. com que API cada operação muda o sistema (PowerShell/`netsh` ou APIs nativas);
2. como impedir que um processo do usuário use o helper elevado como escada de privilégio através da própria raiz (gravável pelo usuário): links em `logs/`, `state/locks/`, `bin/`, troca do instalador entre a verificação e a execução, CA arbitrária em `certs/ca/`;
3. o que fazer com NRPT quando há regras de política de grupo;
4. como embutir o manifest `requireAdministrator` e testar tudo **sem** disparar UAC.

## Decisão
**1. Nenhum shell.** Cada operação usa a API que o cmdlet correspondente usa por baixo, atrás de `forge_core::platform::PrivilegedOps` (`platform/windows/privileged.rs`, Win32/COM só em `platform/windows/`):

| operação | API |
|---|---|
| `nrpt.add` / `nrpt.remove` | WMI, métodos estáticos `Add`/`Remove` de **`PS_DnsClientNrptRule`** em `root/Microsoft/Windows/DNS` (a classe por trás de `Add-DnsClientNrptRule`; `MSFT_DNSClientNRPTRule` em `root/StandardCimv2`, citada no planejamento, **não existe** no Windows 10 19045 — conferido com `Get-CimClass`). Leitura das regras pelo registro (como já fazia o `DnsPolicy`) |
| `hosts.set_block` | arquivo temporário irmão + `ReplaceFileW` (troca atômica que mantém ACL e atributos do `hosts`) |
| flush | `DnsFlushResolverCache` após NRPT e `hosts` (falha vira `details.flush_error`, não erro) |
| `trust.*_machine_root` | `WindowsTrustStore::elevated()` da [ADR-0023](0023-local-ca-name-constraints-trust.md) (parte do registro de `LocalMachine\Root`, `CertAddEncodedCertificateToStore(USE_EXISTING)` / `CertDeleteCertificateFromStore`, busca por `CertThumbprint`); o certificado é lido e conferido por `platform/windows/cert_info.rs` (CryptoAPI: assinatura própria, Basic e Name Constraints) |
| `firewall.*` | COM `INetFwPolicy2`: regra TCP de entrada, programa = exe canônico, porta, grupo `Forgebox`, perfis **privado + domínio** (nunca público) |
| `defender.*` | WMI `MSFT_MpPreference.Add/Remove(ExclusionPath)` em `root/Microsoft/Windows/Defender` |
| `vcredist.install` | ver item 5 |
| `fs.set_case_sensitive` | `CaseSensitiveFs` existente (`NtSetInformationFile`) |

**2. Regras NRPT do Forgebox** têm `Comment = "Forgebox"` (`platform::FORGEBOX_NRPT_COMMENT`, `NrptRule::is_forgebox`) e `DisplayName = "Forgebox <ns>"`. `nrpt.add` é idempotente (regra igual = nada muda) e substitui só regras **locais**, com esse comentário e namespace exatamente `[ns]`; regras de VPN, corporativas ou feitas à mão ficam (listadas em `details.foreign_rules`). `nrpt.remove` idem.

**3. NRPT com política de grupo.** O helper **não** grava na chave de política (`HKLM\SOFTWARE\Policies\...\DnsPolicyConfig`): ela é reescrita pelo próximo `gpupdate` e é território do administrador do domínio. Grava a regra local e responde `effective: false` + `group_policy_rules`, e o daemon cai para o bloco do `hosts` (comportamento já implementado em `forge_net::policy::DnsPolicyManager`, docs/06 §5.4). Isto substitui a obrigação "gravar a regra na tabela efetiva" da ADR-0022.

**4. `trust.add_machine_root` só aceita a CA do Forgebox como a docs/11 §3 a define.** O certificado vem de `certs/ca/*.crt|pem|cer` (PEM ou DER) com o thumbprint pedido e precisa ser autoassinado, `cA = true`, ter CN começando com `Forgebox` e Name Constraints cujos `dNSName` permitidos estejam **todos** sob sufixos de uso local (`dns_name::LOCAL_USE_SUFFIXES`) e cujos `iPAddress` sejam loopback ou privados; outras formas de nome são recusadas (`forge_core::helper::trust`). `trust.remove_machine_root` só remove certificados cujo CN começa com `Forgebox` (ausente = sucesso, `present: false`). Sem isso, qualquer processo do usuário instalaria uma raiz arbitrária para todas as contas da máquina com um único clique no UAC.

**5. `vcredist.install`: verificar e executar a mesma cópia.** `tmp/downloads/` é gravável pelo usuário, então o helper copia o instalador (aberto sem seguir links e sem compartilhamento de escrita/remoção) para `%SystemRoot%\Temp\forgebox-helper-<request_id>\`, diretório **criado já** com DACL protegida `SYSTEM` + `Administrators`, calcula o sha256 durante a cópia, confere a assinatura Authenticode da cópia (`CodeSign`: assinada, confiável, publisher `Microsoft Corporation` = `platform::VCREDIST_PUBLISHER`) e executa a cópia com `/install /quiet /norestart` (`CREATE_NO_WINDOW`, até 15 min). Códigos 0, 3010/1641 (`reboot_required`) e 1638 (igual ou mais novo já instalado) são sucesso; outro código é erro `io`. O diretório é apagado no fim.

**6. Caminhos validados duas vezes.** (a) léxico: `HelperOp::validate` contra uma `HelperPolicy` com a `root` do pedido (já conferida como a mesma pasta do executável, com `canonicalize`) e TLDs/parked dirs do `etc/forgebox.toml` **da raiz do helper**; (b) canônico: todo caminho da operação passa por `canonicalize` (junctions resolvidas, sem `\\?\` via `paths::plain_path`) e a operação é validada de novo contra a raiz e os parked dirs canônicos. Junction `bin/x → C:\Windows\System32`, `tmp/downloads/x → fora` ou `www/link → state/` = `forbidden`.

**7. Arquivos dentro da raiz sem seguir links** (`platform::open_no_follow`): `FILE_FLAG_OPEN_REPARSE_POINT`, recusa reparse point, diretório e arquivo com mais de um hard link; truncamento só depois das checagens. Vale para pedido, resposta, `helper.log`, certificados de `certs/ca/` e o instalador. Os arquivos de troca têm de ser `<raiz>/state/locks/helper-<id>.request.json` e o `.response.json` irmão (diretórios comparados por `canonicalize`); fora disso → código de saída 2, nada é escrito.

**8. Sem log, sem ação.** `logs/helper.log` (JSON lines `{ts, request_id, op, dry_run, ok, code, message}`) é aberto **antes** de ler o pedido: `logs` não pode ser link e o caminho final do handle (`GetFinalPathNameByHandleW`) tem de ser `<raiz canônica>\logs\helper.log`; senão o pedido é recusado (`forbidden`) sem agir. Toda resposta, aceita ou recusada, e todo erro de uso geram uma linha.

**9. Elevação e `--dry-run`.** Sem `--dry-run` o helper exige token elevado (`TokenElevation`) antes de qualquer mudança (`needs_elevation`). Com `--dry-run` executa toda a validação, lê o estado atual (regras NRPT, `hosts`, regras de firewall, `LocalMachine\Root`, flag case-sensitive, hash e assinatura do instalador) e devolve o plano em `details` (`dry_run: true`), sem exigir elevação e sem mudar nada. Pedido pela entrada padrão (`--request -`) só com `--dry-run`; a resposta vai para a saída padrão.

**10. Códigos de saída:** 0 executado/planejado (`ok`), 1 recusado ou falhou (resposta escrita), 2 uso incorreto ou arquivos de troca fora do lugar (nada escrito), 3 resposta não pôde ser escrita.

**11. Manifest** embutido pelo `build.rs` com o crate `embed-manifest` 1.5.1 (MIT, sem dependências, sem `rc.exe`/`mt.exe`; no MSVC passa `/MANIFEST:EMBED /MANIFESTINPUT /MANIFESTUAC:NO` só para os binários): `requestedExecutionLevel level="requireAdministrator" uiAccess="false"`, sem dependência de Common Controls. Toda a lógica fica na lib `forge_helper`; o bin tem `test = false` (um harness de teste do bin herdaria o manifest e o `cargo test` não elevado não conseguiria iniciá-lo). Ícone/VERSIONINFO ficam para o instalador (M4-E1).

**12. Testes sem UAC.** O executável compilado é copiado para uma raiz temporária e iniciado com `__COMPAT_LAYER=RunAsInvoker` (switch de compatibilidade do Windows que roda um programa `requireAdministrator` com o token do chamador, sem prompt). Sem o switch, `CreateProcess` falha com `ERROR_ELEVATION_REQUIRED` (740), o que o teste também confere. O manifesto é lido do `.exe` por `platform::windows::pe_resources` (`LoadLibraryExW` como recurso).

## Alternativas rejeitadas
- **PowerShell (`Add-DnsClientNrptRule`, `Add-MpPreference`) e `netsh advfirewall`:** 0,5–1 s de partida por chamada, saída de texto localizada para interpretar, dependência de política de execução e de `PATH`, e um interpretador inteiro rodando elevado ([11](../11-security.md) §8: "sem shell"). Argumentos validados mitigariam a injeção, mas não o resto.
- **Gravar a NRPT direto no registro:** o serviço DNS Client não é notificado e o formato dos valores não é documentado.
- **`winres`/`winresource`:** exigem `rc.exe` do Windows SDK no build. **`/MANIFESTUAC` manual em `cargo:rustc-link-arg`:** equivalente no MSVC, mas o `embed-manifest` também cobre o alvo GNU e gera o XML completo (compatibilidade, `longPathAware`, UTF-8).
- **Executar o instalador direto de `tmp/downloads/`:** janela entre verificação e execução em pasta gravável pelo usuário.
- **Confiar só no thumbprint de `certs/ca/`:** o arquivo é do usuário; o thumbprint só prova que o daemon e o helper falam do mesmo arquivo, não que ele é a CA restrita.
- **Gravar a regra NRPT na chave de política quando há GPO:** ver item 3.

## Consequências
- `forge-core`: `platform::PrivilegedOps` + `privileged_ops()`, `platform::secure_file` (`open_no_follow`, `final_path`), `platform::windows::{com, wmi, firewall, cert_info, privileged, secure_file, pe_resources}` (o store de `LocalMachine\Root` é o `trust_store.rs` da ADR-0023, mais `machine_root_der`; `CertificateInfo.thumbprint` é o `CertThumbprint` de lá), `process_is_elevated`, `helper::trust`, `hosts::apply_hosts_block_bytes` (bytes fora do bloco preservados mesmo em ANSI), `paths::plain_path`, `NrptRule::is_forgebox`, `VCREDIST_PUBLISHER`/`VCREDIST_INSTALL_ARGS`. Features novas do `windows`: `Win32_System_Wmi`, `Win32_System_Variant`, `Win32_System_Ole`, `Win32_NetworkManagement_WindowsFirewall`.
- Uma CA sem Name Constraints, ou com um TLD público vindo de `[dns] extra_tlds`, não vai para `LocalMachine\Root` (`forbidden`); continua podendo ir para `CurrentUser\Root` sem helper. A CA que o `forge-tls` gera (M1-E4-T4, ADR-0023: CN `Forgebox Local CA ...`, `dNSName` só de uso local, `localhost`, `127.0.0.0/8`, `::1/128`) passa; o teste `the_forge_tls_ca_is_planned_for_the_machine_store` do `forge-helper` roda o binário com essa CA.
- Regras de firewall do Forgebox não valem em redes que o Windows classifica como públicas; o diagnóstico de LAN (M3-E3) deve avisar.
- Mensagens dos providers WMI vêm no idioma do Windows em `message`; a UI traduz pelo `code`.
- **Pendências de release (M4-E1):** (a) compilado sem `+crt-static`, o helper depende de `vcruntime140.dll`; numa máquina sem o runtime VC++ ele não inicia e `vcredist.install` fica inalcançável — os binários de release devem usar `-C target-feature=+crt-static` (não mudado aqui: perfis e flags do workspace são do orquestrador); (b) no modo por usuário ([08](../08-release-layout.md) §2.1) a raiz de dados não é a pasta do executável e o helper recusa o pedido (`forbidden`): o instalador precisa pôr uma cópia do helper na raiz de dados (ou a decisão volta a uma ADR).
- O prompt UAC real e as mudanças de verdade (NRPT, `hosts`, firewall, Defender, `LocalMachine\Root`, VC++) só rodam no smoke manual de M1 (docs/06 §5.5); os testes automáticos cobrem validação, plano (`--dry-run` com leituras reais), recusa sem elevação e o caminho de execução com um sistema falso.
