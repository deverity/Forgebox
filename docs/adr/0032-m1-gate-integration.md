# ADR-0032 — Gate M1: correções de integração, bundle de CAs, ambiente da CLI e escopo do smoke

**Status:** aceito · **Data:** 2026-10-27

## Contexto
O gate M1 (docs/10) foi a primeira execução de ponta a ponta das 24 tarefas de M1: raiz nova, `forge tool install php@8.4 nginx mysql@8.4 mailpit composer`, projeto Laravel, `https://loja.test`, `php artisan migrate` e e-mail no Mailpit, tudo pela CLI (`scripts/smoke.ps1`, [16](../16-smoke-m1.md)). Cada parte tinha testes isolados; juntas, falharam em pontos que nenhum teste unitário cobria. Algumas correções pediram decisões novas, registradas aqui.

Falhas encontradas (todas com teste de regressão):
1. Manifestos `mysql`/`mariadb`/`postgresql` usavam `{{ instance.name }}` em `install.post_install`, que roda uma vez por versão, antes de existir instância: a instalação falhava depois do download.
2. `forge config set ports.ranges.*` gravava o `forgebox.toml`, mas o alocador continuava com as faixas lidas no boot; e a porta `default` do manifesto era usada mesmo fora da faixa configurada.
3. `forge exec -- php` rodava sem `PHPRC`/`PHP_INI_SCAN_DIR` (sem extensões); `forge exec -- composer` não achava o shim `.cmd` (o Rust só procura `.exe`); os shims também não definiam o ambiente.
4. O overlay pedia `extension = bcmath`, que é embutida no PHP 8.4 para Windows (aviso em toda execução).
5. O pool esperava `PHP_VERSION` `8.4.27+1` (zip republicado do provider `php-windows`), o `php-cgi` responde `8.4.27`: nenhum worker subia e o site nunca era renderizado.
6. Rede com inspeção TLS corporativa: `composer create-project` falhava com `curl error 60` (o PHP só confia num arquivo de CAs; o bundle do Composer não tem a raiz da empresa).

## Decisão
**Bundle de CAs do sistema** (`certs/ca-bundle.pem`, `forge_tls::bundle`, [06 §6.5](../06-sites-vhosts-multi-php.md)). `TrustStore::system_roots()` (novo, padrão vazio; Windows: store lógico `Root` do usuário + `LocalMachine\AuthRoot`, só certificados no prazo, sem duplicatas, sem elevação) + a CA local formam um PEM que o ator `tls` reescreve no start e quando a CA muda. O overlay do PHP aponta `openssl.cafile` e `curl.cainfo` para ele.

**Portas.** O alocador guarda `[ports]` num `watch` compartilhado pelos clones (`PortAllocator::set_config`, `in_range`); `Daemon::set_config` repassa. A porta `default` de uma instância só é usada se está livre **e** dentro da faixa configurada; reservas existentes não mudam ([03 §4](../03-engine.md)).

**Variáveis de instalação.** Em `spec.install`, `instance.*` e `prefs.*` são erro de lint (regra 5). Pastas por instância vêm do alvo de render (no start) e de `service.daemon.logs` (o daemon cria a pasta de cada log declarado, dentro da raiz, antes do start).

**Ambiente da CLI** ([06 §2.3](../06-sites-vhosts-multi-php.md), [09](../09-cli.md)). `shell.env` devolve o `config.env` da versão escolhida e `bin/shims` no fim do `PATH`; com versão explícita, `FORGEBOX_<TOOL>_VERSION`. Os shims respeitam essa variável primeiro e chamam `<versão>/forgebox-env.cmd` (gerado com os shims a partir do `config.env`, caminhos relativos ao script). `forge exec` resolve o programa pelo `PATH` + `PATHEXT` do filho.

**Extensões.** `config.extensions.dir` (padrão `ext`); item de `defaults_on` sem arquivo na versão fica fora do overlay. `RenderRequest.files_dir` diz onde os arquivos estão durante a instalação (cópia em staging).

**Versão esperada do pool**: sem metadados de build semver (`8.4.27+1` → `8.4.27`).

**Escopo do smoke.** O gate prova o motor com o que existe em M1: o projeto é criado com `forge exec -- composer create-project` (o `forge new` é M3-E2), o banco com o cliente `mysql` do Forgebox via `forge exec` (os métodos `db.*` são M2-E3) e o `.env` do projeto é editado pelo script (é configuração do app, não do Forgebox; `forge mail env`/`forge db url` são M2-E3). O script nunca escreve configuração do Forgebox nem fala com o daemon fora do `forge`; `curl.exe`/.NET só verificam. A raiz do smoke tem o formato da release (executáveis do workspace copiados para a raiz). Modo `-NonInteractive`: raiz portátil, `curl --resolve` + `--cacert` (nunca `-k`), DNS consultado direto em `127.0.0.1:53`. Modo interativo: raiz instalada (o portátil nunca toca o store de confiança, docs/08 §2.2), `forge dns enable` + `forge tls ca install` e `curl` sem flags; o script nunca roda `forge path add`.

## Alternativas rejeitadas
- *Embutir as raízes da Mozilla (`webpki-roots`) no bundle*: não resolve a inspeção TLS corporativa (a raiz da empresa só existe no Windows) e cria uma lista própria para manter; o Windows já é a fonte de confiança do usuário e do navegador.
- *`disable-tls`/`secure-http: false` no Composer ou `-k` no smoke*: esconde o problema e deixa o PHP sem verificação.
- *Reqwest com raízes próprias*: os downloads do daemon já usam o verificador da plataforma (reqwest 0.13 + rustls-platform-verifier) e passaram na rede corporativa; o problema era só o TLS do PHP.
- *Exigir `forge daemon restart` depois de `forge config set ports.*`*: a API respondia sucesso sem aplicar; o alocador vivo é barato (um `watch`).
- *Gerar o `.env` e o banco por métodos novos (`db.create`, `mail.env`) já no gate*: são tarefas de M2-E3 com UX própria; o gate prova o motor com o cliente oficial do MySQL via `forge exec`, sem atalhos no produto.
- *`PHPRC` no ambiente do usuário (HKCU) ao trocar `current`*: muda o ambiente global do usuário e prende uma série; o script por versão chamado pelos shims não toca nada fora da raiz. `bin/php/current` direto no `PATH` (sem shims) continua sem `PHPRC` — limitação documentada.

## Consequências
- O smoke roda em ~2 min numa raiz limpa (downloads ~350 MB + `composer create-project`) e passa duas vezes seguidas nesta máquina (Laragon ocupando 80/443/3306, inspeção TLS corporativa).
- Raízes que o Windows ainda não baixou (atualização automática) só entram no bundle no próximo start do daemon.
- Shims gravam um arquivo (`forgebox-env.cmd`) dentro da pasta da versão em `bin/`.
- Pendente para o usuário: o modo interativo do smoke (UAC do NRPT e diálogo do `CurrentUser\Root`).
