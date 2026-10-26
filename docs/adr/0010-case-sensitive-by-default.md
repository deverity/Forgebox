# ADR-0010 — Case-sensitive por padrão (paridade Linux)

**Status:** aceito · **Data:** 2026-10-26

## Contexto
Projetos desenvolvidos no Laragon quebram ao publicar em Linux por diferenças de maiúsculas/minúsculas em nomes de arquivos, classes e tabelas. Windows 10 1803+ oferece case sensitivity **por diretório** no NTFS (flag `FILE_CS_FLAG_CASE_SENSITIVE_DIR`, herdada por subdiretórios), sem afetar o resto do sistema.

## Decisão
Forgebox liga a flag em `www/` (e demais parked dirs, `etc/`, `data/`, `logs/`, `tmp/`) no onboarding, oferece conversão reversível para diretórios existentes, gera configs sem regex case-insensitive, tenta `lower_case_table_names=0` em MySQL/MariaDB novos, e expõe um toggle global e por site para desativar. Um "Case Mismatch Finder" cobre os casos em que a flag não pode ser aplicada. Detalhes em [15](../15-linux-parity-case-sensitivity.md).

## Alternativas rejeitadas
- **Só avisar (linter) sem mudar o FS:** o erro continua invisível em runtime.
- **Rodar tudo em WSL:** muda a proposta do produto (nativo Windows) e a experiência de I/O.
- **Default insensitive com opt-in:** quem mais precisa (iniciantes) não ligaria.

## Consequências
- `bin/` fica insensitive (tools Windows não precisam da flag).
- Precisa de diretório vazio para marcar → conversão por "mover para diretório novo".
- Alguns apps Windows antigos podem se comportar de forma estranha dentro de diretórios CS; o toggle por site resolve.
- MySQL `lower_case_table_names=0` em NTFS-CS precisa validação empírica (tarefa no roadmap) e fallback para `2`.
