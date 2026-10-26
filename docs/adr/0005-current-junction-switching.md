# ADR-0005 — Troca de versão ativa por junction `current`

**Status:** aceito · **Data:** 2026-10-26

## Contexto
O usuário já usa no Laragon um script (`switch-php.ps1`) que re-aponta uma junction `bin\php\php` para a versão desejada, de modo que o PATH nunca muda e nenhum terminal precisa ser reiniciado. Alternativas comuns (nvm-windows, scoop) ou reescrevem PATH ou usam shims lentos.

## Decisão
Toda tool com `layout: versioned` tem `bin/<tool>/current` como **junction** (não symlink: não exige privilégio nem Developer Mode). O PATH do usuário recebe `…\bin\<tool>\current` uma única vez. `forge use <tool> <versão>` troca a junction atomicamente (`current.tmp` → rename). Shims `.cmd` opcionais em `bin/shims` para quem não quer PATH por tool; os shims respeitam `.php-version`/`.nvmrc` do diretório atual. Serviços web nunca dependem de `current` (usam caminho exato da versão).

## Alternativas rejeitadas
- **Reescrever PATH a cada troca:** exige novo terminal; polui PATH.
- **Symlink:** requer `SeCreateSymbolicLinkPrivilege` ou Developer Mode.
- **Só shims:** latência por chamada, problemas com ferramentas que resolvem o caminho real (Composer, IDEs).

## Consequências
- IDEs apontam para `…\bin\php\current\php.exe` e seguem a troca automaticamente.
- Junction é por diretório; ferramentas que precisam de `current` dentro de subpasta (ex. `node/current/node_modules`) são resolvidas pelo layout.
