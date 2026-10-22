# Reverse Proxy

Run the server bound to localhost (`AUREN_BIND_ADDR=127.0.0.1:8080`) and terminate TLS at a reverse proxy. **Do not expose the raw bind address publicly** — the application trusts the proxy for TLS and forwarded headers.

## nginx example

```nginx
server {
    listen 443 ssl;
    server_name auren.example.com;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }

    # WebSocket endpoint: requires upgrade headers and a long read timeout
    location /api/v1/events {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_read_timeout 3600s;
    }

    # Skill uploads can be large; align with AUREN_MAX_UPLOAD_BYTES
    client_max_body_size 50m;
}
```

## Caddy example

```caddy
auren.example.com {
    reverse_proxy 127.0.0.1:8080
    request_body {
        max_size 50MiB
    }
}
```

Caddy handles `Connection`/`Upgrade` headers for WebSocket proxies automatically and provisions TLS certificates.

## Notes

- The WebSocket endpoint (`/api/v1/events`) requires `Upgrade`/`Connection` headers and a long read timeout so idle connections are not dropped.
- Keep `client_max_body_size` (or equivalent) aligned with `AUREN_MAX_UPLOAD_BYTES` so clients get a clean 413 from the API rather than a proxy error.
- Health probes (`/health/live`, `/health/ready`) can be exposed to the proxy for load-balancer checks.
