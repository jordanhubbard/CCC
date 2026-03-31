# RCC Hub — HTTPS / TLS Setup

HTTPS is required for any publicly accessible hub. This guide covers two options:

1. **Caddy** (recommended) — automatic cert renewal, zero config
2. **nginx + certbot** — familiar to most sysadmins

Both result in zero manual certificate renewal: certs auto-renew 30 days before expiry.

## Prerequisites

- A domain name pointing to your server (`A` record, propagated)
- Ports **80** and **443** open (in addition to 8789/8788 for direct access)
- RCC already running (see [DOCKER.md](DOCKER.md) or the manual install)

---

## Option 1: Caddy (Recommended)

Caddy is the simplest path — it fetches and renews certs automatically via Let's Encrypt
with no extra tooling.

### Install Caddy

```bash
# Ubuntu / Debian
sudo apt install -y debian-keyring debian-archive-keyring apt-transport-https curl
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | \
  sudo gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | \
  sudo tee /etc/apt/sources.list.d/caddy-stable.list
sudo apt update && sudo apt install caddy
```

### Configure

Edit `Caddyfile` in the rcc-hub directory and replace `rcc.yourdomain.com` with your domain:

```bash
sed -i 's/rcc.yourdomain.com/rcc.example.com/g' Caddyfile
```

### Start

```bash
sudo cp Caddyfile /etc/caddy/Caddyfile
sudo systemctl enable caddy
sudo systemctl restart caddy

# Verify cert obtained:
sudo journalctl -u caddy --since "5 minutes ago"
```

### Verify

```bash
curl https://rcc.example.com/health
# → {"status":"ok",...}
```

Caddy auto-renews certs; no cron or manual steps needed.

---

## Option 2: nginx + certbot

### Install

```bash
sudo apt install -y nginx certbot python3-certbot-nginx
```

### Configure nginx

```bash
sudo cp nginx.conf /etc/nginx/sites-available/rcc

# Edit your domain name
sudo sed -i 's/rcc.yourdomain.com/rcc.example.com/g' /etc/nginx/sites-available/rcc

sudo ln -s /etc/nginx/sites-available/rcc /etc/nginx/sites-enabled/
sudo nginx -t
sudo systemctl reload nginx
```

### Get a certificate

```bash
sudo certbot --nginx -d rcc.example.com
```

certbot will:
1. Verify domain ownership via HTTP challenge
2. Obtain a cert from Let's Encrypt
3. Automatically inject SSL config into your nginx conf

### Auto-renewal

certbot installs a systemd timer (or cron job) that auto-renews before expiry:

```bash
sudo systemctl status certbot.timer
# or check cron:
sudo crontab -l | grep certbot
```

To test renewal:

```bash
sudo certbot renew --dry-run
```

---

## Updating install-hub.sh for TLS

The install wizard in `install-hub.sh` will prompt for TLS setup when your `RCC_PUBLIC_URL`
starts with `https://`. Example snippet already added to the wizard:

```
Would you like to set up HTTPS? [y/N]
 → Installs Caddy and applies Caddyfile automatically
```

Set `RCC_PUBLIC_URL=https://rcc.example.com` in `.env` after TLS is configured.

---

## Firewall (UFW)

```bash
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
# Optionally restrict direct access to RCC ports:
sudo ufw delete allow 8789/tcp
sudo ufw delete allow 8788/tcp
```

---

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `connection refused` on port 80 | Check firewall / security group allows port 80 |
| Caddy: `certificate authority failed` | Check DNS has propagated (`dig +short rcc.example.com`) |
| nginx: 502 Bad Gateway | Confirm `rcc-api` is running on port 8789 |
| cert not renewing | Run `certbot renew --dry-run` and check output |
