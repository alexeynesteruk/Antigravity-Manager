# API Reference (v4.3.0)

This document describes the HTTP API endpoints exposed by **Antigravity Tools** in detail.

> **Note**: as of v4.0.1, all services (including the AI reverse proxy and system management) have been consolidated onto a single port, **8045**. The old port 19527 is deprecated.

## 1. Overview

The Antigravity Gateway is a dual-role server:
1.  **AI Proxy Interface**: a standard interface compatible with the official OpenAI/Anthropic/Google SDKs.
2.  **Management Admin API**: a RESTful interface used to manage accounts, configure the system, and monitor traffic.

### Authentication

| Interface Type | Path Prefix | Auth Method | Header Example | Description |
| :--- | :--- | :--- | :--- | :--- |
| **AI Protocol** | `/v1/*`, `/v1beta/*` | API Key | `Authorization: Bearer <API_KEY>` | used for AI client calls |
| **Admin API** | `/api/*` | Admin Token | `x-admin-token: <TOKEN>` | used for the admin panel or scripted control |

> **Tip**: by default, the `Admin Token` and `API Key` are the same value (i.e. the `API_KEY` you set in `.env` or a Docker environment variable).

---

## 2. Management API

**Base URL**: `http://<host>:8045/api`

### 2.1 Account Management

| Method | Path | Description | Parameter Example |
| :--- | :--- | :--- | :--- |
| **GET** | `/accounts` | get the account list | - |
| **GET** | `/accounts/current` | get the currently active account | - |
| **POST** | `/accounts` | add an account (OAuth Refresh Token) | `{"refreshToken": "..."}` |
| **DELETE**| `/accounts/:id` | delete an account | - |
| **POST** | `/accounts/switch` | switch the active account | `{"accountId": "acc_123"}` |
| **POST** | `/accounts/refresh` | **refresh quota for all accounts** | - |
| **GET** | `/accounts/:id/quota` | **look up a specific account's quota** | - |
| **POST** | `/accounts/:id/toggle-proxy` | disable/enable the account proxy | - |
| **POST** | `/accounts/:id/bind-device` | bind a device fingerprint | `{"mode": "generate"}` |
| **POST** | `/accounts/bulk-delete` | bulk delete accounts | `{"accountIds": ["id1", "id2"]}` |
| **POST** | `/accounts/reorder` | reorder accounts | `{"accountIds": [...]}` |

### 2.2 System Config
| Method | Path | Description |
| :--- | :--- | :--- |
| **GET** | `/config` | get the full configuration |
| **POST** | `/config` | save the full configuration |
| **GET** | `/proxy/status` | get the reverse proxy's running status |
| **POST** | `/proxy/start` | start the reverse proxy service |
| **POST** | `/proxy/stop` | stop the reverse proxy service |
| **POST** | `/proxy/mapping` | update model mapping rules |
| **GET** | `/health` | system health check |

### 2.3 Monitoring & Stats
#### Traffic Logs
*   **GET** `/logs`: get the log list (supports `limit`, `offset`, `filter`, `errorsOnly` parameters)
*   **GET** `/logs/count`: get the total log count
*   **GET** `/logs/:id`: get log details
*   **POST** `/logs/clear`: clear the logs

#### Token Statistics (v4.0.1 New)
*   **GET** `/stats/token/summary`: get a token consumption summary (today/this week/total)
*   **GET** `/stats/token/hourly`: get hourly statistics
*   **GET** `/stats/token/daily`: get daily statistics
*   **GET** `/stats/token/by-account`: consumption breakdown by account
*   **GET** `/stats/token/by-model`: consumption breakdown by model
*   **POST** `/stats/token/clear`: reset statistics data

### 2.4 Advanced Features
*   **POST** `/proxy/cli/sync`: run CLI (Claude/Codex) config file sync
*   **POST** `/accounts/import/db`: import accounts from the old v1 database
*   **POST** `/accounts/oauth/start`: start the OAuth authorization flow (headless)
*   **POST** `/proxy/cloudflared/start`: start a Cloudflare Tunnel

---

## 3. AI Protocol Interface

**Base URL**: `http://<host>:8045`

This service is fully compatible with the official protocol specifications of mainstream AI providers. You can point clients that support OpenAI / Claude directly at this service's address.

### OpenAI Compatible
*   **Chat Completions**
    *   **POST** `/v1/chat/completions`
    *   **Supported models**: any mapped model ID (e.g. `gpt-4o`, `gemini-1.5-pro`)
    *   **Compatibility**: fully compatible with the official OpenAI response format (including streaming SSE).

*   **Image Generation**
    *   **POST** `/v1/images/generations`
    *   **Supported models**: `gemini-3-pro-image` (automatically mapped to Imagen 3)
    *   **Extended parameters**: supports advanced parameters such as `size: "1920x1080"`, `quality: "hd"`.

### Anthropic Compatible
*   **Claude Messages**
    *   **POST** `/v1/messages`
    *   **Purpose**: supports clients such as the Claude CLI (`claude`), Cursor, and Cherry Studio.
    *   **Features**: full support for Tool Use and Thinking mode.

### Gemini Native
*   **Google AI Studio**
    *   **GET/POST** `/v1beta/models/*`
    *   **Purpose**: for applications using the official Google SDK (Python/Node.js).
