#!/usr/bin/env python3
"""DHCPv4 on enp39s0 via AF_PACKET: offer/ack 192.168.11.51 to Cave.

systemd-networkd DHCPServer bound :67 and logged STARTED, but never
OFFERed Cave's DISCOVER. This helper is the speaker-LAN lease.
"""
import socket
import struct
import time

IFACE = "enp39s0"
SERVER = "192.168.11.1"
OFFER_IP = "192.168.11.51"
MASK = "255.255.255.0"
LEASE = 3600
CAVE_MAC = bytes.fromhex("0005cd6d52e8")
BOOTP = "!BBBBIHH4s4s4s4s16s64s128s"
MAGIC = b"\x63\x82\x53\x63"
DISCOVER, OFFER, REQUEST, ACK = 1, 2, 3, 5
ETH_P_ALL = 3
ETH_P_IP = 0x0800


def ip4(s):
    return socket.inet_aton(s)


def srcmac():
    with open(f"/sys/class/net/{IFACE}/address") as f:
        return bytes.fromhex(f.read().strip().replace(":", ""))


def checksum(data):
    if len(data) % 2:
        data += b"\x00"
    s = sum(struct.unpack("!%dH" % (len(data) // 2), data))
    s = (s >> 16) + (s & 0xFFFF)
    s += s >> 16
    return ~s & 0xFFFF


def parse_opts(buf):
    opts = {}
    i = 0
    while i < len(buf):
        t = buf[i]
        if t == 255:
            break
        if t == 0:
            i += 1
            continue
        if i + 1 >= len(buf):
            break
        n = buf[i + 1]
        opts[t] = buf[i + 2 : i + 2 + n]
        i += 2 + n
    return opts


def build_opts(msg_type):
    return b"".join(
        [
            MAGIC,
            bytes([53, 1, msg_type]),
            bytes([54, 4]) + ip4(SERVER),
            bytes([1, 4]) + ip4(MASK),
            bytes([3, 4]) + ip4(SERVER),
            # DNS so Cave can resolve through speaker-LAN NAT when
            # ip_forward is on. Harmless while forwarding is off.
            bytes([6, 8]) + ip4("1.1.1.1") + ip4("9.9.9.9"),
            bytes([51, 4]) + struct.pack("!I", LEASE),
            bytes([58, 4]) + struct.pack("!I", LEASE // 2),
            bytes([59, 4]) + struct.pack("!I", LEASE * 7 // 8),
            bytes([12, 4]) + b"Cave",
            b"\xff",
        ]
    )


def bootp_reply(pkt, msg_type):
    fields = list(struct.unpack(BOOTP, pkt[:236]))
    fields[0] = 2
    fields[8] = ip4(OFFER_IP)
    fields[9] = ip4(SERVER)
    out = struct.pack(BOOTP, *fields) + build_opts(msg_type)
    if len(out) < 300:
        out += b"\x00" * (300 - len(out))
    return out


def ipv4_udp(payload, dst_ip="255.255.255.255"):
    udp_len = 8 + len(payload)
    udp = struct.pack("!HHHH", 67, 68, udp_len, 0) + payload
    tot = 20 + udp_len
    iph = struct.pack(
        "!BBHHHBBH4s4s",
        0x45,
        0,
        tot,
        0,
        0x4000,
        64,
        17,
        0,
        ip4(SERVER),
        ip4(dst_ip),
    )
    iph = iph[:10] + struct.pack("!H", checksum(iph)) + iph[12:]
    return iph + udp


def open_sock():
    s = socket.socket(socket.AF_PACKET, socket.SOCK_RAW, socket.htons(ETH_P_ALL))
    s.bind((IFACE, 0))
    return s


def send_dhcp(s, smac, bootp, dst_mac, dst_ip="255.255.255.255"):
    frame = dst_mac + smac + struct.pack("!H", ETH_P_IP) + ipv4_udp(bootp, dst_ip)
    s.send(frame)


def emit(s, smac, bootp):
    send_dhcp(s, smac, bootp, b"\xff" * 6, "255.255.255.255")
    send_dhcp(s, smac, bootp, CAVE_MAC, "255.255.255.255")
    send_dhcp(s, smac, bootp, CAVE_MAC, OFFER_IP)


print(f"dhcpd {IFACE} offer {OFFER_IP} from {SERVER} (AF_PACKET)", flush=True)
s = None
smac = None
last_req_log = 0.0
while True:
    try:
        if s is None:
            s = open_sock()
            smac = srcmac()
            print("sock open", flush=True)
        pkt = s.recv(2048)
    except OSError as e:
        print(f"sock {e}", flush=True)
        if s is not None:
            try:
                s.close()
            except OSError:
                pass
            s = None
        time.sleep(0.3)
        continue
    if len(pkt) < 42:
        continue
    if struct.unpack("!H", pkt[12:14])[0] != ETH_P_IP:
        continue
    ihl = (pkt[14] & 0xF) * 4
    if pkt[23] != 17:
        continue
    udp = 14 + ihl
    if len(pkt) < udp + 8:
        continue
    sport, dport, ulen, _ = struct.unpack("!HHHH", pkt[udp : udp + 8])
    if dport != 67 or sport != 68:
        continue
    data = pkt[udp + 8 :]
    if len(data) < 240 or data[236:240] != MAGIC:
        continue
    opts = parse_opts(data[240:])
    mt = opts.get(53, b"\x00")[:1]
    mt = mt[0] if mt else 0
    mac = data[28:34]
    if mac[:6] != CAVE_MAC:
        continue
    if mt == DISCOVER:
        kind = ACK if 80 in opts else OFFER
        emit(s, smac, bootp_reply(data, kind))
        print(f"recv DISCOVER xid={data[4:8].hex()} sent {kind} {OFFER_IP}", flush=True)
    elif mt == REQUEST:
        emit(s, smac, bootp_reply(data, ACK))
        now = time.time()
        if now - last_req_log >= 10:
            print(f"recv REQUEST xid={data[4:8].hex()} sent ACK {OFFER_IP}", flush=True)
            last_req_log = now
