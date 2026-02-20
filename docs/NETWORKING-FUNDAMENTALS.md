# Networking Fundamentals — A Visual CLI Tutorial

**Built live from your actual network: aitan-g731 (192.168.50.120)**

---

## Layer 0: The Physical Wire

Everything starts with a physical connection. Your machine has one:

```
$ cat /sys/class/net/enp14s0/speed
1000    ← 1 Gbps (Gigabit Ethernet)

$ cat /sys/class/net/enp14s0/address
d8:43:ae:fa:3f:f5    ← your MAC address (burned into the NIC hardware)
```

```
  ┌──────────────────┐          copper/fiber          ┌──────────────┐
  │   aitan-g731     │ ──────────────────────────────▶ │    Router    │
  │  enp14s0 (1Gbps) │                                │ 192.168.50.1 │
  │  d8:43:ae:fa:3f:f5│                                │ cc:28:aa:5e… │
  └──────────────────┘                                └──────────────┘
```

**What is `enp14s0`?**
- `en` = ethernet
- `p14` = PCI bus 14
- `s0` = slot 0
- This is Linux's predictable naming. Old-style would be `eth0`.

**What is a MAC address?**
- `d8:43:ae:fa:3f:f5` — 6 bytes, globally unique, assigned at the factory
- First 3 bytes (`d8:43:ae`) = manufacturer (OUI). Yours is an ASUS NIC.
- This is the **Layer 2** address — it only matters on your local network segment.

---

## Layer 1: The Local Network (LAN)

Your LAN is `192.168.50.0/24`. That `/24` means:

```
  IP address:    192.168.50.120
  Subnet mask:   255.255.255.0    (24 bits of 1s = /24)
  Network:       192.168.50.0     (the "street name")
  Host part:     .120             (your "house number")
  Broadcast:     192.168.50.255   (shout to everyone on the street)
  Usable range:  192.168.50.1 — 192.168.50.254  (254 possible hosts)
```

```
$ ip addr show enp14s0

  inet 192.168.50.120/24 brd 192.168.50.255 scope global dynamic noprefixroute enp14s0
       │                  │                   │       │
       │                  │                   │       └─ got this from DHCP (not static)
       │                  │                   └─ "global" = routable (not loopback)
       │                  └─ broadcast address
       └─ your IP + subnet mask
```

### Who's on your LAN right now?

```
$ ip neigh show

  192.168.50.1   dev enp14s0  lladdr cc:28:aa:5e:6d:72  REACHABLE   ← router
  192.168.50.3   dev enp14s0  lladdr 40:8d:5c:43:59:58  REACHABLE   ← your other desktop (nixos)
```

This is the **ARP table** — it maps IP addresses to MAC addresses.

```
  YOUR LAN (192.168.50.0/24)
  ═══════════════════════════════════════════════════════════

  ┌─────────────────────┐     ┌─────────────────────┐
  │  aitan-g731 (YOU)   │     │  nixos (other PC)   │
  │  .120               │     │  .3                  │
  │  d8:43:ae:fa:3f:f5  │     │  40:8d:5c:43:59:58  │
  └─────────┬───────────┘     └─────────┬───────────┘
            │                           │
  ══════════╪═══════════════════════════╪═══════════
            │        SWITCH / ROUTER    │
            │    ┌──────────────────┐   │
            └────┤  192.168.50.1    ├───┘
                 │  cc:28:aa:5e:6d:72
                 │  (gateway)       │
                 └────────┬─────────┘
                          │
                     TO INTERNET
```

### How ARP works (the invisible handshake)

When you SSH to `192.168.50.3`, your machine doesn't know its MAC address yet. So:

```
  Step 1: ARP Request (broadcast to everyone on LAN)
  ┌──────────┐  "Who has 192.168.50.3? Tell 192.168.50.120"  ┌──────────┐
  │ .120     │ ──────────── broadcast ──────────────────────▶ │ EVERYONE │
  └──────────┘                                                └──────────┘

  Step 2: ARP Reply (unicast back to you)
  ┌──────────┐  "192.168.50.3 is at 40:8d:5c:43:59:58"       ┌──────────┐
  │ .120     │ ◀─────────── unicast ─────────────────────────  │ .3       │
  └──────────┘                                                └──────────┘

  Step 3: Now your machine caches it in the ARP table
  192.168.50.3 → 40:8d:5c:43:59:58  REACHABLE
```

**Key insight**: IP addresses are for routing across networks. MAC addresses are for delivering frames on the local wire. ARP bridges the two.

---

## Layer 2: IP Routing — How Packets Leave Your LAN

Your routing table tells the kernel where to send packets:

```
$ ip route show

  default via 192.168.50.1 dev enp14s0 proto dhcp src 192.168.50.120 metric 100
  172.17.0.0/16 dev docker0 proto kernel scope link src 172.17.0.1 linkdown
  192.168.50.0/24 dev enp14s0 proto kernel scope link src 192.168.50.120 metric 100
```

Reading this:

```
  DESTINATION          ACTION
  ─────────────────    ──────────────────────────────────────────
  192.168.50.0/24  →   Send directly on enp14s0 (it's local!)
  172.17.0.0/16    →   Send to docker0 bridge (Docker containers)
  default (0.0.0.0)→   Send to 192.168.50.1 (the router/gateway)
```

**The decision tree for every packet:**

```
  Kernel receives packet destined for X.X.X.X
         │
         ▼
  Is X.X.X.X in 192.168.50.0/24?
  ├── YES → send directly via enp14s0 (ARP for MAC, deliver)
  │
  ├── Is X.X.X.X in 172.17.0.0/16?
  │   ├── YES → send to docker0 bridge (container network)
  │   │
  │   └── NO → use default route → send to 192.168.50.1 (router)
  │            Router then forwards it to the internet
  └──
```

**Example**: When you connected to Postgres at `192.168.50.3:5432`:
- Kernel checks: is `192.168.50.3` in `192.168.50.0/24`? **YES**
- Send directly on `enp14s0`, ARP for MAC `40:8d:5c:43:59:58`, deliver

**Example**: When you `curl https://google.com` (IP `142.251.40.238`):
- Kernel checks: is `142.251.40.238` in `192.168.50.0/24`? **NO**
- Is it in `172.17.0.0/16`? **NO**
- Use default route → send to gateway `192.168.50.1`
- Router takes it from there

---

## Layer 3: DNS — Turning Names into Numbers

Before your machine can route a packet to `google.com`, it needs the IP address.

```
$ cat /etc/resolv.conf
  nameserver 192.168.50.1    ← your router is also your DNS resolver

$ dig google.com +short
  142.251.40.238              ← the answer
```

**The DNS resolution chain:**

```
  Your machine                Router (192.168.50.1)         ISP DNS           Root/TLD
  ──────────                  ─────────────────────         ───────           ─────────
       │                              │                        │                  │
       │  "what is google.com?"       │                        │                  │
       ├─────────────────────────────▶│                        │                  │
       │                              │  (checks cache)        │                  │
       │                              │  cache miss →          │                  │
       │                              ├───────────────────────▶│                  │
       │                              │                        │  (recursive)     │
       │                              │                        ├─────────────────▶│
       │                              │                        │◀─────────────────┤
       │                              │◀───────────────────────┤                  │
       │  "142.251.40.238"            │                        │                  │
       │◀─────────────────────────────┤                        │                  │
       │                              │                        │                  │
```

**DNS record types you'll encounter:**
- **A** — name → IPv4 address (`google.com → 142.251.40.238`)
- **AAAA** — name → IPv6 address
- **CNAME** — alias → another name (`www.google.com → google.com`)
- **MX** — mail server for a domain
- **TXT** — arbitrary text (used for SPF, DKIM, verification)
- **NS** — which nameservers are authoritative for a domain

---

## Layer 4: TCP — The Reliable Connection

TCP is how most internet communication works. It guarantees delivery, ordering, and error detection.

### The Three-Way Handshake

Every TCP connection starts with this:

```
  Your machine (.120)                              Google (142.251.40.238)
  ─────────────────                                ────────────────────────
       │                                                  │
       │  SYN  (seq=1000)                                 │
       │  "I want to connect to port 443"                 │
       ├─────────────────────────────────────────────────▶│
       │                                                  │
       │  SYN-ACK  (seq=5000, ack=1001)                   │
       │  "OK, I acknowledge your 1000, here's my 5000"   │
       │◀─────────────────────────────────────────────────┤
       │                                                  │
       │  ACK  (ack=5001)                                 │
       │  "Got it, connection established"                 │
       ├─────────────────────────────────────────────────▶│
       │                                                  │
       │  ═══════ CONNECTION ESTABLISHED ═══════           │
       │  (now send HTTP/TLS data)                        │
```

### Ports — Multiplexing Connections

Your machine has one IP but runs many services. Ports separate them:

```
$ ss -tlnp    (listening TCP sockets on your machine right now)

  Local Address:Port    Process
  ─────────────────     ──────────────────────────────
  127.0.0.1:631     ←  CUPS (printing)
  127.0.0.1:3100    ←  openclaw-gateway (your bot!)
  0.0.0.0:22        ←  SSH server (accepts from anywhere)
  *:11434           ←  Ollama (just installed!)
  127.0.0.1:43147   ←  Windsurf IDE
```

**Key distinction:**
- `127.0.0.1:3100` — only accepts connections from THIS machine (localhost)
- `0.0.0.0:22` — accepts connections from ANY interface (LAN, internet)
- `*:11434` — same as `0.0.0.0`, all interfaces (that's why Ollama is reachable from LAN)

**Port ranges:**
- `0-1023` — well-known (HTTP=80, HTTPS=443, SSH=22, DNS=53)
- `1024-49151` — registered (Postgres=5432, Ollama=11434)
- `49152-65535` — ephemeral (your OS picks these for outgoing connections)

When you `curl https://google.com`:
```
  Source:       192.168.50.120:52847   (random ephemeral port)
  Destination:  142.251.40.238:443    (HTTPS well-known port)
```

---

## Layer 5: Putting It All Together — Anatomy of a Real Request

Here's what actually happened when this machine hit `https://google.com`:

```
$ curl -w 'DNS: %{time_namelookup}s\nConnect: %{time_connect}s\nTLS: %{time_appconnect}s\nTotal: %{time_total}s\n' -so /dev/null https://google.com

  DNS:     0.001629s    ← name resolution (cached, fast)
  Connect: 0.016179s    ← TCP handshake (3-way, ~16ms round trip)
  TLS:     0.046976s    ← TLS handshake (crypto negotiation, ~31ms)
  Total:   0.146518s    ← full request + response
```

**The full journey of that request, step by step:**

```
  TIME        LAYER    WHAT HAPPENS
  ──────────  ───────  ──────────────────────────────────────────────────

  0.000s      DNS      Kernel checks /etc/resolv.conf → asks 192.168.50.1
                       "what is google.com?" → answer: 142.251.40.238

  0.001s      ROUTE    Kernel checks routing table:
                       142.251.40.238 not in 192.168.50.0/24
                       → default route → gateway 192.168.50.1

  0.001s      ARP      Need MAC of gateway → already cached:
                       192.168.50.1 → cc:28:aa:5e:6d:72

  0.001s      L2       Ethernet frame built:
                       src MAC: d8:43:ae:fa:3f:f5 (you)
                       dst MAC: cc:28:aa:5e:6d:72 (router)
                       payload: IP packet to 142.251.40.238

  0.001s      WIRE     Electrical signals on the copper cable → router

  0.002s      ROUTER   Router receives frame, strips Ethernet header,
                       reads IP destination, looks up its own routing table,
                       forwards packet to ISP (Comcast, based on traceroute)

  0.002-      HOPS     Packet traverses the internet:
  0.016s
```

```
  $ traceroute -n 8.8.8.8

   1  192.168.50.1      0.4ms   ← your router
   2  96.120.70.217    17.8ms   ← ISP (Comcast/Xfinity first hop)
   3  24.124.213.21    18.0ms   ← ISP backbone
   4  96.108.45.73     18.0ms   ← ISP backbone
   5  96.110.22.85     19.1ms   ← ISP backbone
   6  96.110.42.9      23.1ms   ← ISP peering point
   7  96.110.34.42     22.8ms   ← ISP → Google handoff
   8  * * *                     ← firewall (drops traceroute probes)
   9  192.178.106.55   18.5ms   ← Google's network
  10  8.8.8.8          17.7ms   ← destination reached
```

```
  0.016s      TCP      SYN packet arrives at Google (142.251.40.238:443)
                       Google sends SYN-ACK back
                       You send ACK → connection established

  0.016-      TLS      Client Hello → Server Hello → certificate exchange
  0.047s               → key exchange → encrypted channel ready
                       (this is why HTTPS is slower than HTTP)

  0.047-      HTTP     GET / HTTP/2 (encrypted inside TLS)
  0.146s               Google sends 301 redirect → www.google.com
                       Total round trip: 146ms
```

---

## Layer 6: Firewalls and Binding — Why Localhost Matters

Remember this from your listening ports?

```
  127.0.0.1:3100    ←  openclaw-gateway (localhost only)
  0.0.0.0:22        ←  SSH (all interfaces)
  *:11434           ←  Ollama (all interfaces)
```

This is **binding** — when a server starts, it chooses which addresses to listen on:

```
  BIND ADDRESS       WHO CAN CONNECT
  ────────────       ──────────────────────────────────────
  127.0.0.1:3100  →  Only processes on THIS machine
  0.0.0.0:22      →  Anyone who can reach any of your IPs
  *:11434         →  Same as 0.0.0.0 — anyone

  192.168.50.3 can SSH to you     (port 22 binds 0.0.0.0)  ✓
  192.168.50.3 can hit Ollama     (port 11434 binds *)      ✓
  192.168.50.3 CANNOT hit gateway (port 3100 binds 127.0.0.1) ✗
```

**This is why the Postgres MCP couldn't connect to 127.0.0.1:5432 earlier** — Postgres was running on the OTHER machine (192.168.50.3), not this one. `127.0.0.1` always means "myself."

And why we had to change the MCP config to `192.168.50.3:5432` — AND the Postgres on that machine had to be configured to listen on `0.0.0.0` (not just `127.0.0.1`) AND its `pg_hba.conf` had to allow connections from `192.168.50.0/24`.

---

## Layer 7: Docker Networking — A Network Inside Your Network

You have Docker running. It creates its own virtual network:

```
  $ ip route show
  172.17.0.0/16 dev docker0 proto kernel scope link src 172.17.0.1 linkdown

  YOUR MACHINE (192.168.50.120)
  ┌──────────────────────────────────────────────────────┐
  │                                                      │
  │   enp14s0: 192.168.50.120  (real network)            │
  │       │                                              │
  │       │   docker0: 172.17.0.1  (virtual bridge)      │
  │       │       │                                      │
  │       │       ├── container A: 172.17.0.2             │
  │       │       ├── container B: 172.17.0.3             │
  │       │       └── container C: 172.17.0.4             │
  │       │                                              │
  │       │   Docker does NAT:                            │
  │       │   172.17.0.x → 192.168.50.120 (masquerade)   │
  │       │                                              │
  └───────┼──────────────────────────────────────────────┘
          │
          ▼  LAN (192.168.50.0/24)
```

**Port mapping** (`-p 5432:5432`) creates a forwarding rule:
```
  Outside world → 192.168.50.120:5432 → docker-proxy → 172.17.0.2:5432
```

That's exactly how Postgres runs on your other desktop — it's a Docker container with port 5432 mapped to the host.

---

## Cheat Sheet: Your Network Right Now

```
  ┌─────────────────────────────────────────────────────────────┐
  │  MACHINE: aitan-g731                                        │
  │  OS: NixOS 25.11                                            │
  │  NIC: enp14s0 (1Gbps)                                      │
  │  MAC: d8:43:ae:fa:3f:f5                                     │
  │  IP: 192.168.50.120/24                                      │
  │  Gateway: 192.168.50.1 (cc:28:aa:5e:6d:72)                  │
  │  DNS: 192.168.50.1                                          │
  │  ISP: Comcast/Xfinity (96.120.x.x first hop)                │
  │                                                             │
  │  LISTENING SERVICES:                                         │
  │    :22      SSH          (all interfaces)                    │
  │    :3100    OpenClaw     (localhost only)                     │
  │    :11434   Ollama       (all interfaces)                    │
  │    :631     CUPS         (localhost only)                     │
  │                                                             │
  │  LAN NEIGHBORS:                                              │
  │    192.168.50.1  → router     (cc:28:aa:5e:6d:72)            │
  │    192.168.50.3  → nixos PC   (40:8d:5c:43:59:58)            │
  │                                                             │
  │  DOCKER: 172.17.0.0/16 (bridge, currently down)              │
  └─────────────────────────────────────────────────────────────┘
```

## Essential CLI Commands Reference

```bash
# See your IP and interfaces
ip addr show

# See your routing table
ip route show

# See who's on your LAN (ARP table)
ip neigh show

# See what ports are listening
ss -tlnp

# Trace the path a packet takes to a destination
traceroute -n 8.8.8.8

# Resolve a domain name
dig google.com +short

# Test if a port is open on a remote host
nc -zv 192.168.50.3 5432

# See active connections
ss -tnp

# Measure connection timing breakdown
curl -w 'DNS:%{time_namelookup} TCP:%{time_connect} TLS:%{time_appconnect} Total:%{time_total}\n' -so /dev/null https://example.com

# Ping (basic reachability test)
ping -c 3 192.168.50.3

# Check DNS configuration
cat /etc/resolv.conf
```
