# forge-helper

**O que é:** Binário `forgebox-helper.exe`: executa, elevado, as operações da allowlist `forge_core::helper::HelperOp` (NRPT, `hosts`, CA em `LocalMachine`, firewall, Defender, VC++, flag case-sensitive), validadas de novo com `HelperOp::validate`, recebendo o pedido e devolvendo a resposta pelos arquivos de troca em `state/locks/` ([ADR-0021](../../docs/adr/0021-elevated-helper-protocol.md)), com log em `logs/helper.log`.

**O que não é:** Não é um serviço do Windows, não aceita comandos fora da allowlist e não define o protocolo (ele vive em `forge-core`, compartilhado com o daemon). Hoje é um placeholder; a execução das operações é M1-E5-T4.

**Quem depende dele:** ninguém diretamente; é invocado sob demanda com UAC (`forge_core::platform::Elevation`).

**Referência:** [docs/11-security.md](../../docs/11-security.md) §1, [ADR-0021](../../docs/adr/0021-elevated-helper-protocol.md).
