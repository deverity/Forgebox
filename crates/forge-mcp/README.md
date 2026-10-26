# forge-mcp

**O que é:** Binário `forge-mcp.exe`: servidor MCP (stdio/WS) que expõe a API do daemon para agentes de IA e IDEs.

**O que não é:** Não tem acesso direto a processos, arquivos ou crates de domínio: usa somente `forge-ipc`.

**Quem depende dele:** ninguém (cliente de agentes).

**Referência:** [docs/01-architecture.md](../../docs/01-architecture.md) §1 e §3.
