# 🐋 Antigravity Manager Native Docker Deployment Guide

This directory contains the native headless Docker deployment for Antigravity Manager. It supports the full Web management UI, API reverse proxy, and data persistence, with no need for a complicated VNC or desktop environment.

## 🆕 New Deployment Option (Reuse a Locally Built Frontend)
Suited to scenarios where "the frontend rarely changes, the backend changes often". The idea is to build `dist/` locally first; Docker then only compiles the backend and copies `dist/` directly, greatly shortening build time and reducing frontend build risk.

**Steps**
1. Generate the frontend static assets locally:
```bash
npm ci --legacy-peer-deps
npm run build
```
2. Build and start with this option (backend-only + reuse `dist/`):
```bash
docker compose -f docker/docker-compose.yml -f docker/docker-compose.localdist.yml build
docker compose -f docker/docker-compose.yml -f docker/docker-compose.localdist.yml up -d
```
Or combine into a single command:
```bash
docker compose -f docker/docker-compose.yml -f docker/docker-compose.localdist.yml up -d --build
```

Watch the logs live after starting:
```bash
docker compose -f docker/docker-compose.yml -f docker/docker-compose.localdist.yml logs -f --tail=200
```

**How to update**
- Backend changed: rerun `build` + `up -d` above
- Frontend changed: run `npm run build` locally again first, then rerun `build` + `up -d`

**Git deployment reminder**
- If the server does not build the frontend locally, make sure `dist/` has been committed to the repo (it has been removed from `.gitignore` in this version).

## 🚀 Quick Start

### 1. Pull the Image Directly (Recommended)
You can pull the pre-built image directly from Docker Hub and start it without fetching the source code:

> [!IMPORTANT]
> **Security warning**: starting from v4.0.3, the Docker build supports **separating the admin password from the API Key**:
> *   **API Key**: set via `-e API_KEY=xxx`, used to authenticate all AI protocol API calls.
> *   **Web admin password**: set via `-e WEB_PASSWORD=xxx`, used only for Web UI login.
> *   **Default behavior**: if `WEB_PASSWORD` is not set, the system automatically falls back to using `API_KEY` as the login password. If neither is set, a random key is generated.
> *   **How to view it**: run `docker logs antigravity-manager` and look for `Current API Key` or `Web UI Password`, or run `grep -E '"api_key"|"admin_password"' ~/.antigravity_tools/gui_config.json` to check.

```bash
# Start the container (replace your-secret-key with a strong key)
docker run -d \
  --name antigravity-manager \
  -p 8045:8045 \
  -e API_KEY=your-api-key \
  -e WEB_PASSWORD=your-login-password \
  -e ABV_MAX_BODY_SIZE=104857600 \
  -v ~/.antigravity_tools:/root/.antigravity_tools \
  lbjlaq/antigravity-manager:latest
```

#### 🔐 Authentication Logic (Security Scenarios)
*   **Scenario A: only `API_KEY` is set**
    - **Web login**: use `API_KEY` to log into the admin panel.
    - **API calls**: use `API_KEY` to authenticate AI requests.
*   **Scenario B: both `API_KEY` and `WEB_PASSWORD` are set (recommended)**
    - **Web login**: `WEB_PASSWORD` **must** be used. Entering the API Key will now be rejected, keeping admin access and API access isolated.
    - **API calls**: continue using `API_KEY`. You can safely distribute the API Key to team members while keeping the password for admin use only.

#### 🆙 Upgrading From an Older Version
If you're upgrading from an older version, `WEB_PASSWORD` is not set by default. You can add it in one of these ways:
1.  **Web UI (recommended)**: log in with the existing `API_KEY`, then set a new admin password on the **API Reverse Proxy** settings page.
2.  **Environment variable**: stop the old container and add `-e WEB_PASSWORD=your-new-password` when starting the new one.

> [!TIP]
> **Priority Logic (Priority)**:
> - The **environment variable** (`ABV_WEB_PASSWORD` / `WEB_PASSWORD`) has the highest priority. If it is set, the program always uses it, ignoring the value in the config file.
> - The **config file** (`gui_config.json`) is used for persistent storage. When you change and save the password via the Web UI, the new password is written to this file (the JSON field name is `admin_password`).
> - **Fallback**: if neither of the above is set, it falls back to `API_KEY`; if even `API_KEY` is not set, one is generated randomly.

### 2. Using Docker Compose
Run this in the `docker` directory:
```bash
docker compose up -d
```

### 3. Building the Image Manually (Developers)
If you need to modify the code or customize the build, run this in the project root:
```bash
# Build with the default "latest" tag
docker build -t antigravity-manager:latest -f docker/Dockerfile .
```

#### 💡 Build Arguments
This image supports automatic mirror source switching, to speed up builds in mainland China:
*   `USE_MIRROR`:
    *   `auto` (default): automatically detects the network environment; if Google is unreachable, switches to a mainland China mirror (Alibaba Cloud / NPM Mirror).
    *   `true`: force using the mainland China mirror source.
    *   `false`: force using the official default source.

Example:
```bash
# Force using the mainland China mirror to speed up the build
docker build --build-arg USE_MIRROR=true -t antigravity-manager:latest -f docker/Dockerfile .
```

## ⚙️ Environment Variable Configuration

| Variable Name | Default | Description |
| :--- | :--- | :--- |
| `PORT` | `8045` | port the service listens on inside the container |
| `ABV_API_KEY` | - | **[Important]** the proxy API key. The key clients (e.g. Claude Code) must provide when accessing |
| `ABV_WEB_PASSWORD` | - | **[Security]** the Web admin panel login password. Falls back to the API Key if not set |
| `ABV_MAX_BODY_SIZE` | `104857600` | **[Performance]** maximum request body size limit (bytes). Default 100MB, used to resolve 413 errors on large image uploads |
| `LOG_LEVEL` | `info` | log level (debug, info, warn, error) |
| `ABV_DIST_PATH` | `/app/dist` | path where frontend static assets are served from (already built into the Dockerfile) |
| `ABV_PUBLIC_URL` | - | public URL used for remote OAuth callbacks (optional) |

## 📂 Data Persistence
Make sure to mount a host directory to `/root/.antigravity_tools` inside the container, otherwise accounts and configuration will be lost when the container restarts.

## 🌐 Access URLs
*   **Admin UI**: [http://localhost:8045](http://localhost:8045)
*   **API Base**: [http://localhost:8045/v1](http://localhost:8045/v1)

## 📦 Docker Hub Distribution (Recommended)
To push to your own repository:
```bash
# Tag the version and push
docker tag antigravity-manager:latest lbjlaq/antigravity-manager:latest
docker tag antigravity-manager:latest lbjlaq/antigravity-manager:4.3.0
docker push lbjlaq/antigravity-manager:latest
docker push lbjlaq/antigravity-manager:4.3.0
```
