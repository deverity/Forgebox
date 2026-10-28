# ADR-0018 — Índice do registry assinado com minisign destacado sobre os bytes crus

**Status:** aceito · **Data:** 2026-10-27

## Contexto
M1-E1-T3 assinava o índice sobre o **YAML emitido pelo `serde_yaml_ng`** a partir da struct re-parseada (sem `spec.signature`), com a assinatura dentro do próprio `index.yaml`. O verificador, no Forgebox do usuário, re-serializava com a versão de `serde_yaml_ng` compilada nele. Qualquer mudança de estilo do emissor (aspas, indentação, quebras, escapes) ou troca da biblioteca (o [ADR-0012](0012-yaml-crate-serde-yaml-ng.md) escolheu uma crate que pode ser substituída) invalidaria em silêncio todas as assinaturas já publicadas, e o registry oficial e os de terceiros parariam de carregar até serem reassinados. Além disso o formato não era reproduzível fora do Forgebox: autores de registries de terceiros e o bot do registry (docs/05 §6) não conseguiam usar `minisign -S`. Nenhum registry foi publicado ainda, então a troca custa pouco agora e viraria migração de formato depois.

## Decisão
1. A assinatura é um arquivo **`index.yaml.minisig`** ao lado de `index.yaml`, no formato padrão do minisign (prehashed), e o **payload é exatamente o conteúdo bruto de `index.yaml`**. Sem canonicalização, sem re-serialização, sem dependência do emissor YAML.
2. `spec.signature` é removido do schema (`Signature`/`SignatureType`/`DEV_KEY_ID` deixam de existir; `deny_unknown_fields` recusa um índice antigo). O `key_id` exibido é o da chave pública configurada que verificou; a própria assinatura minisign carrega o key id e a verificação recusa outra chave.
3. Cada fonte entrega os bytes intactos: `Dir` lê os dois arquivos; `Http` baixa `<URL do index.yaml>.minisig` (404 = sem assinatura) antes de qualquer manifesto; `Git` faz checkout byte-exato (`core.autocrlf=false`, `core.eol=lf`; o repositório do Forgebox fixa LF em `.gitattributes`); o cache grava e reverifica o par.
4. A verificação roda sobre os bytes **antes** de interpretá-los como YAML. Regras de confiança inalteradas: sem `.minisig` só carrega com `trusted_unsigned`; assinatura presente e inválida é sempre recusada; assinatura sem chave configurada só carrega com `trusted_unsigned`.
5. Ferramenta: `cargo xtask registry-sign --key <secreta> [--dir <registry>]` escreve o `.minisig` (o mesmo que `minisign -S -m index.yaml` produziria, com outro comentário) e confere com o loader antes de gravar. Um teste golden fixa o sha256 do `index.yaml` da fixture e verifica a assinatura commitada com `registry/keys/dev.pub`, para que uma atualização de dependência que mude a semântica da verificação apareça nos testes.

## Alternativas rejeitadas
- **JSON canônico (RFC 8785/JCS) do índice parseado, sem `spec.signature`:** independe do emissor YAML, mas exige implementar/importar JCS, fixar o formato de `updated_at` e de números, e mantém a assinatura dentro do arquivo; continuaria sem funcionar com o `minisign` padrão.
- **Manter o YAML emitido pelo `serde_yaml_ng`:** acopla toda assinatura publicada a uma versão de biblioteca (o problema original).
- **Assinar cada manifesto:** o índice já contém o sha256 de cada um; uma assinatura sobre o índice cobre todos com uma verificação.

## Consequências
- Editar `index.yaml` depois de assinar, até por um espaço, invalida a assinatura; `registry-index` e `registry-sign` são sempre usados em sequência (o lint acusa um `.minisig` velho).
- Quem entrega o arquivo (CDN, proxy, checkout) não pode normalizar quebras de linha; quem espelhar um registry deve copiar os bytes.
- Qualquer pessoa assina e verifica com o minisign padrão; registries de terceiros não precisam do Forgebox para publicar.
- Migração: não há registry publicado; `registry/index.yaml` e as fixtures foram reassinados neste commit.
