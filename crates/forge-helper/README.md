# forge-helper

**O que é:** Binário `forgebox-helper.exe` (lib `forge_helper` + bin): executa, elevado, **uma** operação da allowlist `forge_core::helper::HelperOp` por processo (NRPT, `hosts`, CA em `LocalMachine\Root`, firewall, Defender, VC++, flag case-sensitive). Lê o pedido e escreve a resposta nos arquivos de troca de `state/locks/` ([ADR-0022](../../docs/adr/0022-elevated-helper-protocol.md)), valida de novo (política lida da própria raiz, caminhos revalidados depois de resolver junctions), exige token elevado e age pelas APIs de `forge_core::platform::PrivilegedOps` (WMI/COM/CryptoAPI, sem shell; [ADR-0024](../../docs/adr/0024-helper-execution.md)). Tudo vai para `logs/helper.log`. `--dry-run` valida e devolve o plano sem mudar nada. Manifest `requireAdministrator` embutido pelo `build.rs` (`embed-manifest`).

**O que não é:** Não é um serviço do Windows, não aceita comandos fora da allowlist, não tem Win32 próprio (está em `forge-core/src/platform/windows/`) e não define o protocolo (ele vive em `forge_core::helper`, compartilhado com o daemon).

**Quem depende dele:** ninguém diretamente; o daemon o inicia sob demanda com UAC (`forge_core::platform::Elevation`, `HelperClient`).

**Testes sem UAC:** `cargo test -p forge-helper` (fakes na lib; binário real copiado para uma raiz temporária e iniciado com `__COMPAT_LAYER=RunAsInvoker`); `cargo test -p forge-helper --test cli -- --ignored vcredist_dry_run_online` baixa o `vc_redist.x64.exe` real para `Forgebox-wt/cache` e confere hash + Authenticode em `--dry-run`. Manifest: `mt.exe -inputresource:target\debug\forgebox-helper.exe;#1 -out:con`.

**Referência:** [docs/11-security.md](../../docs/11-security.md) §1, [ADR-0022](../../docs/adr/0022-elevated-helper-protocol.md), [ADR-0024](../../docs/adr/0024-helper-execution.md).
