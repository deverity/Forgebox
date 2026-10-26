# ADR-0017 — Verificação e extração em Rust puro (rpgp, minisign-verify, zip 9, sevenz-rust2, lzma-rust2) e MSRV 1.88

**Status:** aceito · **Data:** 2026-10-26

## Contexto
M1-E2-T2 precisa verificar downloads (sha256/sha512, PGP destacado de nginx/Node, minisign, Authenticode) e extrair zip/7z/tar.gz/tar.xz com proteção contra zip-slip e bombas de descompressão, no Windows, sem toolchain C extra, dentro da política de licenças do `deny.toml` e com `cargo deny` limpo. As versões das crates que corrigem advisories conhecidos exigem Rust 1.88; o workspace declarava 1.85.

## Decisão
1. **PGP:** crate `pgp` (rpgp) 0.21, Rust puro. Chaves confiáveis vêm de keyrings embutidos (`crates/forge-provider/keys/<nome>.asc`, escolhidos por nome: `nginx`, `nodejs`) ou de texto armored de fonte confiável (registry). Modelo de confiança: toda chave primária do keyring é âncora (fingerprints fixados por teste e comentados no `.asc`); subchave só vale com binding válido da primária (incluindo back-signature de subchave de assinatura) e sem revogação; primária com assinatura de revogação é descartada. Certificações de user ID não são avaliadas (assinaturas de terceiros são normais); expiração não é avaliada (rpgp não tem motor de política) — os keyrings só trazem chaves listadas como atuais pelo upstream e o arquivo já está fixado por sha256.
2. **Minisign:** `minisign-verify` (MIT, sem dependências), só assinaturas *prehashed* (padrão do minisign ≥ 0.8), verificadas em streaming no mesmo passe do sha256.
3. **Authenticode:** trait `forge_core::platform::CodeSign`; implementação Windows com `WinVerifyTrust` (`WINTRUST_ACTION_GENERIC_VERIFY_V2`), primeiro assinatura embutida e depois catálogos do sistema (`CryptCATAdmin*`, SHA-256 e SHA-1) — binários do Windows como `notepad.exe` são assinados por catálogo. Publisher por `CryptQueryObject` + `CertGetNameStringW`. **Sem checagem de revogação** (`WTD_REVOKE_NONE`): funciona offline e nunca trava em CRL/OCSP; o sha256 do registry continua sendo a garantia primária.
4. **Arquivos:** `zip` 9 (só `deflate`/`deflate64`), `sevenz-rust2` 0.20 (fork mantido do `sevenz-rust`; sem features padrão), `tar` + `flate2` (miniz_oxide) + `lzma-rust2` 0.16 para xz (a mesma implementação LZMA que o `sevenz-rust2` usa). A extração é nossa: nunca `ZipArchive::extract`/`Entry::unpack`. Cada entrada é validada antes de escrever; zip e 7z são varridos por inteiro antes do primeiro byte; staging em `tmp/stage/<ulid>` com a sensibilidade a maiúsculas do pai do destino; rename atômico (cópia + conferência + rename entre volumes).
5. **MSRV 1.88** (`rust-version` do workspace e `msrv` do clippy): exigido por `zip` ≥ 9 e `pgp` ≥ 0.18. O toolchain continua `stable`. Com MSRV 1.88 o clippy passa a sugerir *let-chains* (`collapsible_if`); o lint fica `allow` no workspace até uma tarefa de limpeza migrar o código, para não quebrar branches em andamento escritas para 1.85.
6. **Advisories ignorados com motivo** no `deny.toml`: `RUSTSEC-2023-0071` (rsa, Marvin: canal lateral de tempo em operações com chave **privada**) e `RUSTSEC-2026-0387` (crypto-bigint 0.5, aritmética não constante): o Forgebox só verifica assinaturas com chaves públicas sobre dados públicos. Revisitar quando o rpgp migrar para `rsa`/`elliptic-curve` corrigidos.

## Alternativas rejeitadas
- **sequoia-openpgp:** LGPL-2.0+ (fora da allowlist de licenças) e backend nettle exige toolchain C/cmake no Windows (o backend CNG é experimental).
- **`gpg.exe`/`7z.exe` como subprocesso:** não fazem parte da instalação, dependeriam do PATH do usuário e de parsing de saída.
- **`xz2`/`liblzma`:** compilam liblzma em C (`liblzma-sys`); `xz2` está sem manutenção.
- **`sevenz-rust` 0.6:** parado; o fork `sevenz-rust2` corrige bugs e compartilha o `lzma-rust2`.
- **Manter MSRV 1.85 com `zip` 7.3 + mais três ignores** (RUSTSEC-2026-0367/0368/0369, que afetam só `extract()`): funcionaria, mas manteria uma versão vulnerável justamente no caminho de segurança da extração.
- **Declarar 1.85 e depender de crates 1.88:** metadado falso.
- **Features `bzip2`/`zstd`/`lzma` do zip:** bzip2 traz licença `bzip2-1.0.6` (fora da allowlist), zstd exige C; zips de tools Windows usam deflate.

## Consequências
- Limitações conhecidas: 7z com BCJ2/coders de múltiplas entradas ou AES não extrai (erro `verification_failed` com a mensagem da crate); zip criptografado é recusado; tar com links (simbólicos ou físicos), devices ou sparse é recusado — inclusive tarballs Linux reais como `node-*-linux-x64.tar.xz` (aceitável: o alvo v1 é Windows).
- O writer do `sevenz-rust2` 0.20 grava o bitset de anti-items invertido; o extrator ignora anti-items (valida o nome e não age), como o 7-Zip ao extrair.
- Tempo de modificação: zip usa campos NTFS/extended timestamp; sem eles, o horário DOS (sem fuso) é tratado como UTC.
- Duplicatas de RustCrypto (`sha2` 0.10/0.11, `digest`...) aparecem como avisos de `multiple-versions` até o rpgp acompanhar.
