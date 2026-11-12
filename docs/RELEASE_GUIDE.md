# Antigravity Tools 发版操作指南 (Release SOP)

本项目默认仅运行轻量验证。推送 `v*` 标签只校验版本与通道，**不会自动构建安装包、构建 Docker 镜像或发布 Release**。需要云端打包时由维护者在 Actions 中明确选择；无需云端打包的日常开发不产生这些任务。

## 日常 CI

`.github/workflows/ci.yml` 对 `main`、`beta` 的 push 和 PR 提供一个固定名称的 `Validate Linux` 检查：

- 每次运行先使用系统 Node.js 执行内置 CI 辅助脚本测试，无需安装 npm 依赖；随后根据 Git 完整差异选择前端、后端或两者。文档变更跳过应用检查；无法确定差异或手动运行时执行两者。
- 前端使用 Node.js 24、锁文件缓存和 `npm ci --legacy-peer-deps`。`npm run build` 已包含 TypeScript 检查，只运行一次。
- 后端安装 Linux 开发依赖，使用 Rust 缓存，运行 `cargo fmt -- --check` 与 `cargo clippy --locked --all-targets --all-features`。Clippy 编译测试目标但不执行测试；不重复运行 `cargo check`。后端单独检查时提供临时前端占位文件供 Tauri 编译。
- 新 push 取消同一分支或 PR 的旧 CI。检查任务本身不会因工作流级路径过滤而消失；更改所需检查规则时使用 `Validate Linux`。
- 日常 CI 不构建 Tauri 安装包、Android 应用、macOS 或 Windows 应用。

Pages 仅在 `main` 的 `web_site/**` 或 Pages 工作流变更时自动部署，也可从 `main` 手动运行。

## 版本、通道与发布前检查

| 通道 | 分支 | 标签 | 发布后的更新目标 |
| --- | --- | --- | --- |
| 正式版 | `main` | `vX.Y.Z` | GitHub Latest、Docker `latest` |
| 预览版 | `beta` | `vX.Y.Z-beta.N` 等合法预发布标签 | GitHub prerelease 与 `preview/updater.json`；不更改任何 stable latest 标签 |

新功能或重大改动遵循根目录 `AGENTS.md` 的维护者暂存规则。发布前对**将要打标签的精确提交**执行规定预检；日常修改不执行完整发布预检：

```bash
cd src-tauri
cargo fmt -- --check
cargo clippy --locked --all-targets --all-features
cd ..
npm ci --legacy-peer-deps
npm run build
```

本机缺少目标平台依赖或工具链时，按 `AGENTS.md` 将对应打包验证交由手动云端任务完成。

## 准备版本与更新日志

1. 在相应的 `main` / `beta` 发布分支运行 `npm run bump <patch|minor|major|beta|version>`。可先使用 `--dry-run` 查看影响；执行前保持工作区干净。脚本同步 manifests、锁文件和已有的双语日志文件，不创建另一种历史布局。
2. 当前布局：英文 `CHANGELOG.md`、中文 `CHANGELOG_ZH.md`；旧上游布局：英文 `CHANGELOG_EN.md`、中文 `CHANGELOG.md`。两种布局均受支持。每个已存在日志都必须包含与标签**完全一致**的非空版本章节。
3. 骨架只是占位内容。发布前用 Git 历史与已合入 PR 核对真实变更、作者、co-author 和关联 Issues/PRs；每个相关条目注明来源，外部贡献者在条目内写 `(Thanks to @username)`，仅使用已核实且相关的署名。
4. 正式版同步 `README.md`（英文）与 `README_ZH.md`（中文）的版本与更新摘要。预发布只更新日志，README 保持最新正式版。
5. 按项目纪律创建有 Issue/PR 关联、范围清晰的提交。审核通过后由维护者推送分支与标签；本指南不授权自动推送。

```bash
# 正式版从 main；预览版从 beta。示例版本必须与所有 manifests 和日志一致。
git tag vX.Y.Z
git push origin vX.Y.Z
```

标签工作流检查现有标签、精确 checkout、目标分支祖先关系、六处 manifest/lockfile 版本和日志章节。缺失或不一致直接失败，不使用占位 Release notes。

## 明确选择手动打包或发布

Actions → **Release** → **Run workflow**，选择目标通道分支（`main` / `beta`）或现有版本标签，填写 `release_tag`。若留空则使用所选 ref 的标签或 package.json 版本；相应版本标签必须已经存在且指向当前 checkout。

| 输入 | 默认值 | 行为 |
| --- | --- | --- |
| `build_native` | `false` | 构建六个平台产物：macOS arm64 / x64 / universal、Linux x64 / arm64、Windows x64 |
| `publish_release` | `false` | 需要 `build_native=true` 与 updater 签名密钥；完整产物验证后发布 GitHub Release |
| `build_docker` | `false` | 构建 amd64 / arm64 Docker 镜像；未选择发布时上传 OCI 归档供下载 |
| `publish_docker` | `false` | 需要 `build_docker=true` 与 `DOCKERHUB_USERNAME` / `DOCKERHUB_TOKEN`；推送版本化架构标签并在两者成功后发布多架构 manifest |

全部保持默认值时只运行一个校验任务，不创建 Release。仅构建时提供短期 Actions artifacts，不创建公开 Release，也不修改更新通道。Native 与 Docker 可独立选择；Docker 发布不依赖 GitHub 安装包发布。

选中的 tag/ref 必须包含本次 `scripts/ci/` 辅助脚本更新；更早历史标签会在打包前给出明确错误。历史安装包仍可从已有 Release 下载。

### 签名、完整性与失败处理

- 原生公开发布要求 `TAURI_SIGNING_PRIVATE_KEY`，可同时配置 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。没有密钥时只允许未签名的下载产物，不生成公开 updater manifest。
- 签名必须与当前应用内置公钥对应；工作流检查 SHA-256 与产物来源 metadata 一致，并使用 Node.js 内置 Ed25519 / BLAKE2b 校验 updater 主产物及 trusted comment 的 minisign 签名、算法和 key ID，验证通过后才创建 Release。此检查不替代实际安装验证；有意轮换密钥时需另行安排可信旧公钥的过渡流程。
- 六个目标必须全部成功，来源标签与 commit 必须一致。updater 中的五个架构 URL 从实际资产文件名生成；universal macOS 保留供人工下载。
- 已签名安装包、AppImage 和 app archive 不做二次修改。macOS 归档仅改上传文件名，不改内容。
- 先创建/复用未发布的 draft，上传完整资产和 `updater.json`，全部成功后再发布。已公开的同名 Release 拒绝覆盖；失败的上传保留 draft 供检查，不发布残缺集合。
- beta Release 标记 prerelease 且 `latest=false`，仅在成功发布后更新独立 `preview/updater.json`。正式版才设置 latest。preview 的归档 URL 指向版本化 prerelease。
- Docker 仅先推送版本化架构标签。全部架构成功后才生成版本 manifest；正式版随后更新 `latest` / `latest-amd64` / `latest-arm64`。预发布不更改这些标签。架构失败可能留下版本化单架构镜像，但不会前移 latest。
- GitHub、preview 和 Docker Hub 是独立 API，跨服务更新不具备事务性。发现通道更新失败时检查日志并由维护者修复，不能假定全部发布步骤同时成功。
- 发布通道使用同一 concurrency group，避免同时改写。GitHub 每组只保留一个等待运行；连续提交多个手动发布时检查是否有等待任务被替换，逐一运行。

### 只构建 Windows

Actions → **Build Windows Executable (manual)**。默认构建选中的分支并上传 NSIS 安装包，不发布。

`create_release` 默认 `false`；显式开启时必须提供已存在的 `release_tag`，通过相同版本与通道门禁后仅创建/更新 **DRAFT** 并附上当前日志和 Windows 产物，不公开发布，也不生成或修改 updater 通道。需要完整公开 Release 时使用上面的 Release 工作流。两个路径都拒绝覆盖已公开的版本。

## 验证工作流维护改动

```bash
node --test scripts/ci/helpers.test.mjs
actionlint -shellcheck= -pyflakes=
```

辅助脚本测试使用临时目录与伪产物，不调用 GitHub/Google API，不运行应用构建，不修改仓库版本。对锁文件安装、各原生平台打包、签名和 Docker 发布的端到端验证仅在维护者明确选择相应任务后执行。

仅 `.github/workflows/` 下的文件会被 GitHub 执行。根目录 `workflows/` 是非活动历史副本，不应复制覆盖当前配置。

参考：[GitHub 工作流语法与路径过滤](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax)、[concurrency 行为](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-workflow-concurrency)、[setup-node 缓存](https://github.com/actions/setup-node)、[Artifact 完整性与布局](https://github.com/actions/download-artifact)、[Ubuntu 24.04 运行器软件](https://github.com/actions/runner-images/blob/main/images/ubuntu/Ubuntu2404-Readme.md)、[GitHub CLI Release](https://cli.github.com/manual/gh_release_create)。
