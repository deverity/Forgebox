# ADR-0009 — API "remote-ready" desde a v1

**Status:** aceito · **Data:** 2026-10-26

## Contexto
Requisito futuro: instalar o motor em Linux (ou outro Windows) e controlá-lo a partir da GUI/CLI de outra máquina ([13](../13-remote-mode.md)). Também: métricas por vhost em stream ([14](../14-observability-metrics.md)). Ambos dependem da forma da API, que é cara de mudar depois que GUI, CLI e MCP a consomem.

## Decisão
Na v1, mesmo só com transporte local, a API segue regras "remote-ready":
1. `forge-ipc` expõe um trait `Transport` (NamedPipe, UnixSocket, WebSocketTls, SshTunnel) e o cliente é agnóstico.
2. Identidade por ULID; caminhos de arquivo são atributos, nunca chaves.
3. Streams (eventos, logs, métricas) têm `seq` monotônico, assinatura com filtro e `resume_from(seq)`.
4. Métodos idempotentes com `request_id`.
5. Acesso a arquivos via métodos da API (`fs.*`) restritos a `etc/`, `logs/`, `www/`, nunca caminho arbitrário.
6. Métodos que dependem do SO do cliente são marcados `local_only` nos contratos.
7. Autorização por perfil (`admin|operator|viewer`) já modelada nos contratos, mesmo que local seja sempre `admin`.

## Alternativas rejeitadas
- **Fazer remoto via SSH + CLI apenas (sem API remota):** perde GUI, eventos em tempo real e métricas.
- **Adiar tudo:** reescrever contratos depois de M2 custaria mais que as 7 regras acima.

## Consequências
- Um pouco mais de rigor em M1-E5-T1. Nenhum custo em runtime local.
- Build Linux do daemon vira trabalho de `platform::linux` + providers, não de API.
