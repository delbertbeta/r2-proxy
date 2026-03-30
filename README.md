# R2 代理服务器

一个高性能的 Rust S3 代理服务器，用于代理 Cloudflare R2 存储的请求。

## 功能特性

- 🔒 **白名单验证**: 通过 Cloudflare KV 验证 bucket 访问权限
- 🌐 **CORS 支持**: 动态从 KV 获取并应用 CORS 配置
- ⚡ **高性能**: 支持流式传输，节省内存和存储资源
- 🛡️ **安全**: 只允许 GET 和 OPTIONS 请求
- 📊 **日志**: 完整的请求日志和错误追踪
- 🖥️ **Status 页面**: 启动时同时暴露独立监控端口，查看流量、缓存、错误和热点排行

## 环境变量配置

创建 `.env` 文件并配置以下环境变量：

```env
# 服务器配置
PORT=3000
STATUS_PORT=3001
STATUS_HOST=127.0.0.1
STATUS_API_KEY=change-me

# Redis 配置
REDIS_URL=redis://127.0.0.1:6379
REDIS_KEY_PREFIX=r2proxy

# Cloudflare 配置
CLOUDFLARE_ACCOUNT_ID=your_cloudflare_account_id
CLOUDFLARE_API_TOKEN=your_cloudflare_api_token
CLOUDFLARE_KV_NAMESPACE_ID=your_kv_namespace_id

# Cloudflare R2 配置
R2_ENDPOINT=https://your-account-id.r2.cloudflarestorage.com
R2_ACCESS_KEY_ID=your_r2_access_key_id
R2_SECRET_ACCESS_KEY=your_r2_secret_access_key

# 本地磁盘缓存配置（可选）
LOCAL_CACHE_ENABLED=true
LOCAL_CACHE_MAX_SIZE=1G
LOCAL_CACHE_DIR=/var/cache/r2-proxy
```

- `STATUS_HOST` 默认是 `127.0.0.1`，也就是 status 服务默认只监听本机
- `STATUS_PORT` 默认是 `3001`
- `STATUS_API_KEY` 是 status 页面和 `/api/*` 接口的访问密钥
- `REDIS_URL` / `REDIS_KEY_PREFIX` 现在是全局配置，同时用于本地缓存元数据和 status 指标存储
- `LOCAL_CACHE_MAX_SIZE` 支持自然语言容量，例如 `512M`、`1G`、`1024K`
- 任何以 `index.html` 结尾的对象路径都不会进入本地缓存
- 每个响应都会带 `X-R2-Proxy-Cached`，取值为 `HIT`、`MISS`、`BYPASS`、`DISABLED`
- 本地磁盘只保存响应体，缓存元数据、响应头和 LFU 索引存放在 Redis 中
- 如果 Redis 不可用，本地缓存会自动降级为禁用状态，请求继续回源
- status 指标也使用 Redis 保存：
  - 全量累计指标：总请求数、总流量、缓存命中率、错误数/错误率
  - 最近 7 天明细：`1h` 按 `5m`、`24h` 按 `1h`、`7d` 按 `1d`
  - Top10 榜单：热缓存文件、最多 miss URL、最多错误 URL

## Status 页面

服务启动后会同时监听两个端口：

- `PORT`：现有代理服务
- `STATUS_HOST:STATUS_PORT`：status 页面和监控 API

页面功能包括：

- 总请求数
- 总流量大小
- 总缓存命中率
- 总错误数 / 错误率
- 本地缓存占用大小 / 使用率
- `1h / 24h / 7d` 的 QPS、吞吐、缓存命中率、错误率折线图
- 最近 7 天 Top10：
  - 最热缓存文件
  - 最多 miss 的请求 URL
  - 最多错误的请求 URL
- 支持按虚拟 bucket 过滤

### 访问方式

1. 打开 `http://127.0.0.1:3001/`（如果改了 `STATUS_HOST` / `STATUS_PORT`，按你的配置访问）
2. 首次打开输入 `STATUS_API_KEY`
3. 页面验证成功后会把 key 保存在浏览器 `localStorage`
4. 之后页面会自动带上 `X-Status-API-Key`

如果 API key 变更或输入错误，页面会自动清掉本地缓存的 key 并回到登录页。

### API 示例

```bash
curl -X POST http://127.0.0.1:3001/api/login \
  -H 'content-type: application/json' \
  -d '{"apiKey":"change-me"}'
```

```bash
curl http://127.0.0.1:3001/api/overview \
  -H 'X-Status-API-Key: change-me'
```

```bash
curl 'http://127.0.0.1:3001/api/timeseries?range=24h&bucket=@' \
  -H 'X-Status-API-Key: change-me'
```

## Cloudflare KV 配置

### 白名单配置

在 KV 中存储 bucket 白名单，键格式：`whitelist:{bucket_name}`，值为 `true` 或 `false`。

示例：
```
whitelist:my-bucket = true
whitelist:another-bucket = false
```

### CORS 配置

在 KV 中存储 CORS 配置，键格式：`cors:{bucket_name}`，值为 JSON 格式的 CORS 配置。

示例：
```json
{
  "allowed_origins": ["*"],
  "allowed_methods": ["GET", "OPTIONS"],
  "allowed_headers": ["*"],
  "expose_headers": [],
  "max_age": 86400,
  "allow_credentials": false
}
```

### SPA 配置

在 KV 中存储 SPA 配置，键为 `spa`，值为 JSON 对象，格式如下：

```json
{
  "foo": true,
  "@": false
}
```

- key 为映射前的虚拟 bucket 名
- value 为 `true` 时，该域名开启 SPA 模式
- 开启后，如果请求路径没有文件名后缀，则直接回源根目录 `index.html`

## Bucket Mapping

- 访问 mybucket.delbertbeta.life/path 代表虚拟 bucket "mybucket"，访问 delbertbeta.life 代表虚拟 bucket "@"
- 实际访问的 S3 bucket 由 KV 中 whitelist 的 value 决定。例如：
  - `whitelist` = `[["foo", "real-bucket-name"], ["@", "default-bucket"]]`，则 foo.delbertbeta.life 代理到 S3 的 real-bucket-name，delbertbeta.life 代理到 default-bucket
- CORS 配置由 KV 中 cors 的 value 决定，格式为 `{ "bucketname": { ...CORS配置... } }`
- SPA 配置由 KV 中 spa 的 value 决定，格式为 `{ "bucketname": true }`
- 如果没有配置或 value 为空，则拒绝访问

## API Usage

### Request Format

```
GET https://[bucket].delbertbeta.life/{path}
```

### Example Request

```bash
# Get file
curl https://my-bucket.delbertbeta.life/images/logo.png

# Preflight request
curl -X OPTIONS https://my-bucket.delbertbeta.life/images/logo.png
```

## 构建和运行

### 开发环境

```bash
# 安装依赖
cargo build

# 运行开发服务器
cargo run
```

启动后日志中会同时看到代理端口和 status 端口的监听信息。

### 生产环境

```bash
# 构建发布版本
cargo build --release

# 运行生产服务器
./target/release/r2-proxy
```

如果你需要把 status 页面暴露给外部访问，可以把 `STATUS_HOST` 改成 `0.0.0.0`，但仍然建议放在反向代理或内网环境后面。

## 项目结构

```
src/
├── main.rs          # 主程序入口
├── config.rs        # 配置管理
├── errors.rs        # 错误处理
├── cors.rs          # CORS 配置
├── stats.rs         # Redis 指标采集和查询
├── status_server.rs # Status API 和页面服务
├── status_assets.rs # 嵌入式前端资源
├── local_cache.rs   # 本地磁盘缓存
├── kv_client.rs     # Cloudflare KV 客户端
└── s3_client.rs     # S3/R2 客户端
```

## 性能优化

- 使用流式传输减少内存占用
- 异步处理提高并发性能
- 连接池复用减少连接开销
- 错误缓存避免重复请求

## 安全考虑

- 只允许 GET 和 OPTIONS 请求
- 通过 KV 白名单验证 bucket 访问权限
- 动态 CORS 配置防止跨域攻击
- 完整的错误处理和日志记录

## 许可证

MIT License

## Docker Usage

### Build image (if local build)
```bash
docker build -t r2-proxy .
```

### Run with .env file (recommended)
```bash
docker run -d -p 3000:3000 --name r2-proxy --restart unless-stopped --volume .env:/app/.env delbertbeta/r2-proxy:latest
```

### 或直接用环境变量
```bash
docker run --rm \
  -p 3000:3000 \
  -p 3001:3001 \
  -e PORT=3000 \
  -e STATUS_PORT=3001 \
  -e STATUS_HOST=0.0.0.0 \
  -e STATUS_API_KEY=change-me \
  -e REDIS_URL=redis://redis:6379 \
  -e CLOUDFLARE_ACCOUNT_ID=xxx \
  ... \
  delbertbeta/r2-proxy:latest
```
