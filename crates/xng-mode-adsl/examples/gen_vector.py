# Independent ADS-L vector generator, mirroring the OGN/SoftRF C struct
# (ads-l.h ADSL_Packet) and ognconv.cpp XXTEA + PolyPass CRC. Separate
# implementation/language from the Rust crate; its output bytes are pinned
# into the Rust decode test, and the asserted physical values come from the
# EASA spec field encodings (worked examples), not from a Rust loopback.

def uns_encode(value, N):
    # inverse of spec uns_decode: value = 2^e*(2^N+base)-2^N
    Thres = 1<<N
    for e in range(4):
        lo = ((Thres)<<e) - Thres   # value at base=0 for this exponent
        # max value for this e: base = Thres-1
        hi = ((Thres + (Thres-1))<<e) - Thres
        if lo <= value <= hi:
            base = ((value + Thres) >> e) - Thres
            return (e<<N) | (base & (Thres-1))
    # saturate
    return (3<<N) | (Thres-1)

def sign_encode(value, N):
    sign = 1 if value < 0 else 0
    mag = uns_encode(abs(value), N)
    return (sign<<(N+2)) | mag

# --- XXTEA key0, from ognconv.cpp ---
DELTA = 0x9e3779b9
M32 = 0xFFFFFFFF
def mx_key0(y, z, s):
    return ((((z>>5) ^ ((y<<2)&M32)) + ((y>>3) ^ ((z<<4)&M32))) ^ (((s^y)&M32) + z)) & M32
def encrypt_key0(data, loops):
    n=len(data); s=0; z=data[n-1]
    for _ in range(loops):
        s=(s+DELTA)&M32
        for p in range(n-1):
            y=data[p+1]
            data[p]=(data[p]+mx_key0(y,z,s))&M32; z=data[p]
        y=data[0]
        data[n-1]=(data[n-1]+mx_key0(y,z,s))&M32; z=data[n-1]
    return data

# --- CRC PolyPass, from ads-l.h ---
POLY=0xFFFA0480
def poly_pass(crc, byte):
    crc = (crc | byte) & M32
    for _ in range(8):
        if crc & 0x80000000: crc ^= POLY
        crc = (crc<<1)&M32
    return crc
def calc_crc(data):
    crc=0
    for b in data: crc=poly_pass(crc,b)
    crc=poly_pass(crc,0);crc=poly_pass(crc,0);crc=poly_pass(crc,0)
    return crc>>8

# === Build the descrambled 20-byte payload per the OGN struct layout ===
payload = [0]*20

# Type = 0x02 iConspicuity
payload[0] = 0x02

# Address: AMT=5 (ICAO), Address=0x3C5EE2 (an ICAO-style 24-bit), relay=0.
# get4bytes(Address) LE word = byte1 | byte2<<8 | byte3<<16 | byte4<<24
# getAddress=(word>>6)&0xFFFFFF ; getAddrTable=byte1&0x3F ; relay=byte4&0x80
AMT = 5
ADDR = 0x3C5EE2
relay = 0
word = (AMT & 0x3F) | ((ADDR & 0xFFFFFF) << 6) | ((relay & 1) << 39)
payload[1] = word & 0xFF
payload[2] = (word>>8) & 0xFF
payload[3] = (word>>16) & 0xFF
payload[4] = (word>>24) & 0xFF

# Meta (bytes 5..6): byte5 = TimeStamp[6] | FlightState[2]<<6
#                    byte6 = AcftCat[5] | Emergency[3]<<5
TIMESTAMP_Q = 40          # 10.0 s
FLIGHT_STATE = 2          # airborne
ACFT_CAT = 4              # glider
EMERGENCY = 1             # no emergency
payload[5] = (TIMESTAMP_Q & 0x3F) | ((FLIGHT_STATE & 0x3) << 6)
payload[6] = (ACFT_CAT & 0x1F) | ((EMERGENCY & 0x7) << 5)

# Position (bytes 7..17): Lat[24] Lon[24] Speed[8] Alt[14] Climb[9] Track[9]
# Lat raw24 = round(lat_deg * 93206) ; Lon raw24 = round(lon_deg * 46603)
import math
LAT_DEG = 47.5            # Switzerland-ish
LON_DEG = 8.5
lat_raw = round(LAT_DEG * 93206) & 0xFFFFFF
lon_raw = round(LON_DEG * 46603) & 0xFFFFFF
payload[7]  = lat_raw & 0xFF
payload[8]  = (lat_raw>>8) & 0xFF
payload[9]  = (lat_raw>>16) & 0xFF
payload[10] = lon_raw & 0xFF
payload[11] = (lon_raw>>8) & 0xFF
payload[12] = (lon_raw>>16) & 0xFF

# Speed: use spec worked example 0xc4 = 120 m/s
payload[13] = 0xc4

# Alt: use spec worked example 0x0528 = 1000 m (14-bit field)
# Stored: Position[7]=low byte, Position[8] = (Position[8]&0xC0) | (word>>8)
# Here Position index is 0-based within the 11-byte Position; mapping to payload:
# pos[0]=payload[7]...pos[7]=payload[14], pos[8]=payload[15], pos[9]=payload[16], pos[10]=payload[17]
ALT_WORD = 0x0528
payload[14] = ALT_WORD & 0xFF             # pos[7]
# pos[8] low 6 bits hold alt high; high 2 bits hold climb low 2 bits (set later)
payload[15] = (ALT_WORD>>8) & 0x3F        # pos[8] alt portion

# Climb: spec worked example 0x048 = +10 m/s (9-bit). 
# climb_word stored across pos[8] bits6-7 (low 2 bits of climb_word) and pos[9] low 7 bits.
CLIMB_WORD = 0x048
payload[15] = (payload[15] & 0x3F) | ((CLIMB_WORD & 0x3) << 6)   # pos[8] high 2 bits
payload[16] = (CLIMB_WORD >> 2) & 0x7F                            # pos[9] low 7 bits

# Track: 9-bit. track_word = pos[10]<<1 | pos[9]>>7
# Choose track 90.0 deg -> word = round(90/(360/512)) = 128
TRACK_WORD = 128
payload[16] = (payload[16] & 0x7F) | ((TRACK_WORD & 0x1) << 7)    # pos[9] bit7
payload[17] = (TRACK_WORD >> 1) & 0xFF                            # pos[10]

# Integrity (bytes 18..19): byte18 = SIL[2] | DA[2]<<2 | NIC[4]<<4
#                           byte19 = NACp[3] | GVA[2]<<3 | NACv[2]<<5
SIL=3; DA=2; NIC=11; NACp=6; GVA=3; NACv=2
payload[18] = (SIL&0x3) | ((DA&0x3)<<2) | ((NIC&0xF)<<4)
payload[19] = (NACp&0x7) | ((GVA&0x3)<<3) | ((NACv&0x3)<<5)

# === Scramble payload into 5 LE words, then assemble frame ===
words = [ payload[i*4] | (payload[i*4+1]<<8) | (payload[i*4+2]<<16) | (payload[i*4+3]<<24) for i in range(5)]
encrypt_key0(words, 6)
scrambled = []
for w in words:
    scrambled += [w&0xFF, (w>>8)&0xFF, (w>>16)&0xFF, (w>>24)&0xFF]

VERSION = 0x00
LENGTH = 24
# CRC covers Version + scrambled payload (the 21 bytes), per setCRC (TxBytes-6 = 21)
crc_data = [VERSION] + scrambled
crc = calc_crc(crc_data)
crc_bytes = [(crc>>16)&0xFF, (crc>>8)&0xFF, crc&0xFF]

# Two framings: with and without the leading Length byte.
frame_no_len = [VERSION] + scrambled + crc_bytes
frame_with_len = [LENGTH] + frame_no_len

print("FRAME_WITH_LEN =", "[" + ", ".join("0x%02x"%b for b in frame_with_len) + "]")
print("len with =", len(frame_with_len), "no =", len(frame_no_len))
print()
print("EXPECTED VALUES:")
print("address = 0x%06X" % ADDR, "(%d)"%ADDR)
print("amt =", AMT)
print("timestamp_q =", TIMESTAMP_Q, "-> s =", TIMESTAMP_Q*0.25)
print("flight_state =", FLIGHT_STATE)
print("aircraft_category =", ACFT_CAT)
print("emergency =", EMERGENCY)
print("lat_raw = 0x%06X" % lat_raw, "-> deg =", (lat_raw if lat_raw<0x800000 else lat_raw-0x1000000)/93206)
print("lon_raw = 0x%06X" % lon_raw, "-> deg =", (lon_raw if lon_raw<0x800000 else lon_raw-0x1000000)/46603)
print("speed (0xc4) =", "spec 120 m/s")
print("alt (0x0528) =", "spec 1000 m")
print("climb (0x048) =", "spec +10 m/s")
print("track_word =", TRACK_WORD, "-> deg =", TRACK_WORD*360/512)
print("SIL/DA/NIC =", SIL, DA, NIC)
print("NACp/GVA/NACv =", NACp, GVA, NACv)
