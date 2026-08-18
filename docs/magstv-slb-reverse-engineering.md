# MAGSTV / Xuper TV — conocimiento de ingeniería inversa

> Estado al 2026-08-18. Documento de referencia técnica sobre el protocolo del
> proveedor, obtenido por análisis estático y dinámico (Frida) de la app
> oficial `com.android.mgstv` 4.99.5 (Xuper TV) y su librería nativa
> `libranger-jni.so`. Complementa a `magstv-consolidation-handoff.md`
> (estado del proyecto) con el detalle de protocolo y la metodología.
>
> **Redacción**: este documento no contiene credenciales, tokens de sesión,
> licencias ni identificadores de cuenta; todos los valores de ese tipo
> aparecen como marcadores (`<token_portal>`, `<user_id>`, …). Las constantes
> de protocolo (claves públicas del servidor, secretos de revisión de la app)
> sí se incluyen porque van empaquetadas en el plugin de todas formas.

## 1. Arquitectura descubierta

Dos planos separados:

- **Plano de control (portal)**: HTTPS a dominios rotados (p.ej.
  `ogvkxy.4kcvozfrt.com`), endpoints `api/portalCore/v<N>/<acción>`, cuerpos
  JSON cifrados con 3DES-ECB (clave derivada del metadata `PORTAL_KEY` del
  APK). Login, catálogo live/VOD, EPG, `getLiveData` v7, `startPlayVOD` v10,
  `getItemData` v4, `getSlbInfo` v15.
- **Plano de datos (SLB)**: HTTP/1.1 en claro a gateways
  (`208.115.243.51:30111` y fronts Cloudflare con subdominios aleatorios).
  **Toda** la media (playlists m3u8 y segmentos `video/MP2T`) sale del
  gateway; no existe CDN directo abierto (verificado: 100 % del tráfico de
  media en las capturas va al gateway).

La app corre un **relay local** nativo (`127.0.0.1:4xxxx`): el player pide
`GET /live/0/<play_code>.ts` al relay y el motor nativo lo traduce a
peticiones envueltas contra el gateway. La URL del relay llega a Java en
`Status.play_url` (callback `OnPrepareEvent`).

Flujo extremo a extremo:

1. login portal (DoHttpSec) → token de sesión portal;
2. `getSlbInfo` v15 → tabla de gateways/CDNs (`main_slb_addr`,
   `spared_slb_addr`, `main_slb_token`, `spared_slb_token`, `dead_time_addr`)
   que Java pasa al nativo con `SetEntries`;
3. `getLiveData` v7 → por canal `live_address_list[]` con `play_code`
   (`cyx_<hex>_720p`) y `license` (`app_id&tag=free&scheme=md5-01&media_code=
   …&expired=…&token=<32hex>`);
4. `PrepareProgram(play_code, license)` al motor nativo;
5. el nativo envuelve cada petición media (§3) y fetchea del gateway;
6. el player consume del relay local.

**Por qué falla nuestro portal directo**: todas las llamadas del portal de la
app van por `DoHttpSec`, que añade las cookies de sesión SLB; el portal
devuelve `rc=1` a clientes sin esa sesión. El bloqueo nunca fue la firma
`sign2` (§4): es el transporte SLB.

## 2. Datos clave de protocolo (constantes de revisión)

- Alfabeto base64 propietario (paths y cookies; sin padding):
  `jWB7YtC3n9iXbEkUcJl1VxF4STpQoOIaRmh2M-efAgLwPqGr6uyD5vNsdH_Kz0Z8`
- Clave pública X25519 estática del servidor (en el binario):
  `edc005119b59a57f36fc3e045c9105f8c3582653c4546d557beb3a0d59c67735`
- Secreto `sign_o3` (21 bytes): ASCII `salt3333=4` +
  `98 0d 0a 15 32 c9 c3 82 17 08 c0`
  (hex: `73616c74333333333d34980d0a1532c9c3821708c0`)
- Versiones de API observadas: `startPlayLive` v5 (la app **no la usa**),
  `getLiveData` v7, `startPlayVOD` v10, `getItemData` v4, `getSlbInfo` v15.
- UA interno: `Ranger/4.13.9-e84208b9`; motor "Titan" 2.9.12.
- Handshake de sesión: rutas `/slb/v13/live` y `/slb/v13/vod`
  (clase `SlbV13Protocol`; también existe `SlbV10Protocol`).

## 3. El sobre SLB por petición (cookies d/s/t)

En el cable, cada petición media es:

```
GET /<path señuelo aleatorio> HTTP/1.1
Host: 208.115.243.51:30111
Connection: Keep-Alive
Cookie: d=<~1067 chars>; s=<46 chars>; t=<43 chars>
```

- El path es **señuelo** (1-3 segmentos, nuevo por petición incluso para la
  misma playlist). La URI real viaja cifrada dentro de `d`.
- `d` = base64-propietario de **AES-CBC-128** (PKCS7) de un bloque de
  cabeceras (~800 bytes). Clave e IV **constantes por sesión** (prefijo de
  cifrado común de 416 bytes entre peticiones lo demuestra). Origen de la
  clave/IV: pendiente (§6).
- `s` (decodifica a 34 bytes) y `t` (32 bytes) son **constantes por PROCESO
  de la app**, generadas en el Init nativo de cada proceso (corregido el
  2026-08-18 tarde: dos procesos del mismo install tienen `s`/`t` distintas;
  la constancia matinal era del mismo proceso longevo). La clave AES del
  sobre es por tanto también por proceso, derivada en el Init nativo —
  candidata: DH X25519 con la clave pública estática del servidor
  (`SecHttpClient::Initialize`), aún no capturado en vivo (el único DH visto
  es del módulo P2P).

Bloque de cabeceras en claro (plaintext de `d`, plantilla verificada; CRLF):

```
App: com.android.msandroid
App-Version: 49905
Content-Auth: /live/?user_id=<user_id>&trans_id=<rid>&app_id=com.android.msandroid&host=<gateway>&app_ver=49905&client_ip=<egress_mx>&expired=<+4h>&auth_id=<…>&dev_id=<dev_id>&tag=free&sign_ver=1&token=<token_portal>&sign2_method=sign_o3&instance=0&start_moment=<ms_epoch>&sign2=<sign_o3(§4)>
Content-License: <licencia del portal verbatim: app_id&tag&scheme=md5-01&media_code=<play_code>&expired&token>
Ranger-Id: <id de instalación, 30 chars alfabeto propio>
User-Agent: Ranger/4.13.9-e84208b9
X-Buffer: 16168974193
uri: /live/<play_code>.m3u8            (o /live/<play_code>/<…>_shisui_<id>.ts para segmentos)
```

**Anti-replay**: sobre bien formado pero de un solo uso. Reenviar una petición
capturada segundos antes (mismo egress, mismo wire format) → `400` y cierre.
Petición bien formada con recurso inválido → `404` (distingue sobre válido de
inválido). Las respuestas media van **en claro** (m3u8, MP2T).

## 4. `sign2` / `sign_o3` — RESUELTO Y VERIFICADO

**Input** (byte-exact, capturado en el md5-update nativo):

```
"token=" + <token_portal 32hex> + "&sign2_method=sign_o3&instance=0&start_moment=" + <ms epoch> + <secreto 21 bytes §2>
```

Padding y estado inicial MD5 estándar.

**Digest**: MD5 con la primera ronda usando el orden de palabras rotado 10
(`[10,11,12,13,14,15,6,7,8,9,0,1,2,3,4,5]`); el resto equivale a MD5
estándar. La implementación que el plugin ya tenía reproduce los vectores en
vivo — fue verificada byte a byte (12 vectores capturados, fuzz 200/200
contra un emulador del asm, 5/5 contra la función nativa invocada vía Frida
`NativeFunction`). El secreto de 21 bytes está empaquetado en el plugin
(`SignO3Signer::for_supported_revision`) con test de 5 vectores vivos.

Hay **dos** MD5 custom en la librería: la de `sign_o3` (@ `0x63d900`) y otra
para dns/stats/p2p (@ `0x63e71c`, distinta). No confundir.

## 5. Lo que NO es el cifrado del portal

- Cuerpos del portal: 3DES-ECB (`jb/b.java`), clave derivada de
  `PORTAL_KEY` del manifest (el plugin ya lo implementa).
- Canal Java↔JNI: AES-128-CBC con clave aleatoria por proceso e IV=clave
  (`ec/a.java`), base64 con el alfabeto propio (`ec/b.java`). Solo protege el
  IPC interno, no el protocolo.

## 6. Abierto (siguiente trabajo)

1. **Clave AES+IV del sobre `d`**: pendiente. Hipótesis ordenadas:
   a. deriva de los tokens de `getSlbInfo` (portal) — comprobable offline:
      llamar `getSlbInfo` v15 desde el provider y probar derivaciones contra
      una cookie `d` capturada (plaintext conocido);
   b. X25519 con la clave estática del servidor + KDF en
      `SecHttpClient::Initialize` @ `0x41c4a8` (el DH capturado en runtime
      resultó ser del módulo P2P, no del canal media);
   c. IV: ¿ceros, derivado de clave, o aleatorio enviado al servidor?
2. **Semántica exacta de `s` y `t`** (t = 32 bytes sugiere una clave pública
   X25519 por instalación; no coincide con derivaciones simples de `dev_id`).
3. **Handshake `/slb/v13/*`**: formato request/response (probablemente h2 con
   curl+BearSSL embebidos; el handshake puede llevar la clave efímera del
   cliente y devolver material de sesión).
4. **Ticket Murmur3** (`MurmurHash3_x64_128` @ `0x63c09c`, builder @
   `0x399c24` sobre `http://encrypt.io?scheme=aes-cbc-128-o2&key=<K>&resource=
   <R>`, seed `0x5f325ab6`): rol en el sobre o en el path señuelo.
5. **Vigilancia de rotación**: la app anuncia v5.1; congelar la revisión
   soportada y preparar re-verificación de vectores al cambiar.

## 6b. Actualización 2026-08-18 (tarde): el directorio SLB y el bootstrap

Nuevos hechos verificados tras §1-6:

- **El portal habla HTTP/2** (host `ftmrmy.jdfey0cd.com`), no el HTTP/1.1 que
  usa nuestro provider; aun así responde a nuestro cliente (`rc=0` en login,
  catálogo y `getSlbInfo`).
- **`getSlbInfo` v15 funciona desde el provider** (rc=0 con `userToken` +
  `userId` en el bean — sin ellos da `rc=1`), pero devuelve `cdn_list` vacía:
  la respuesta real está personalizada por la **sesión SLB** (las peticiones
  de la app van envueltas por `DoHttpSecP` con cookies `d/s/t`).
- **El directorio SLB real** (capturado en el `SetEntries` Java→nativo): 8
  entradas (`live`×3, `vod`×3, `record`, `short`) con tags `icdn`/`cf`/
  `google`, dominios main/spare por entrada y un query string `auths` por
  entrada con `session_id` (12 chars), `auth_id`, `media_encrypted=0`,
  `client_ip`, `sign_type` (`cs`/`cfl`/`goog`), `group` (hex largo),
  `ctrl_type=account`, `app_ver`, `dev_id`. El handshake del canal live va a
  `dgggy78.dcoynuhet.com` (host `main_addr` de la entrada live/icdn).
- **El bootstrap de sesión es el handshake `/slb/v10/live`** (HTTP/2, justo
  después de `getSlbInfo`): es la raíz de la cadena — con sesión establecida,
  el portal personaliza y el gateway acepta los sobres. Capturar su
  request/response es el siguiente paso.
- `SetEnv` lleva un `communication_key` (UUID) generado en Java por
  instalación; no deriva la cookie `t` por las vías simples probadas
  (sha256/md5 de communication_key, android_id, dev_id y combinaciones).
- El único DH X25519 observado en runtime es del **módulo P2P**, no del canal
  media: la sesión media no se renegocia por proceso (material por
  instalación o derivado de identidad).
- **El handshake `/slb/v10/live` quedó capturado** (2026-08-18 tarde, app con
  datos borrados): POST h2 a `eijbs.gn5h3hxar2k.com` con `content-length:
  640` y cookies `d/s/t` ya presentes (el sobre protege TODAS las peticiones,
  incluidas portal y handshake — no hay bootstrap sin sobre). La secuencia de
  primer arranque: `googleadservices` → `v8/active` → `getSlbInfo` →
  `getColumnContents` → `/slb/v10/live` → `getLiveData`.
- **Corrección**: `s`/`t` son por proceso (Init nativo), no por instalación.
- El DH X25519 del canal media sigue sin capturarse: ocurre en el Init
  nativo (primer segundo del proceso) y no usa el wrapper libsodium visible
  en los hooks tardíos.

## 7. Metodología (para futuras revisiones)

### Laboratorio

- Contenedor `magstv-redroid-030` (redroid, aarch64) con la app oficial;
  tráfico enrutado por el sidecar `jellyrin-magstv-probe-magstv-egress-1`
  (WireGuard `mx`, salida MX). Si falta conectividad:
  `ip route replace default via 172.20.0.2 dev eth0` y
  `ndc resolver setnetdns 100 "" 172.20.0.2` dentro del contenedor.
- `frida-server` 16.6.6 dentro del contenedor
  (`/data/local/tmp/frida-server-16.6.6 -l 0.0.0.0:27042`); cliente
  `/tmp/magstv-frida166-venv/bin/python`, conexión a `172.20.0.3:27042`.
- Trampas conocidas: `input keyevent` falla intermitente → usar
  `sh /system/bin/input keyevent N`; el contenedor no arranca si el host
  pierde `/dev/input` (`mkdir /dev/input`); tras `docker restart` hay que
  rehacer ruta/DNS; el proceso de la app se llama `com.android.mgstv`
  (UI "Xuper"); esperar `getprop sys.boot_completed` = 1 antes de `am start`.
- El login de la app exige el **ID numérico** de la cuenta (el campo valida
  6-12 dígitos), no el email.

### Técnicas que funcionaron

- **Desencriptado estático de strings**: las cadenas sensibles van XOR byte a
  byte en `.data` y se desencriptan one-shot en el prólogo de cada función;
  emular el patrón `ldrb/eor/strb` recupera el 100 % (artefacto:
  `libranger-dec.so`).
- **Emulador aarch64 acotado** (`/tmp/slbwork/emulate_transform.py`): ejecuta
  la función objetivo del asm (incluidas las ramas de ofuscación con tablas
  de 2 entradas) sobre entradas elegidas. Permitió verificar el digest
  byte a byte sin entender cada paso.
- **Oráculo nativo**: `NativeFunction` de Frida para invocar la función real
  con entradas elegidas y comparar con la reimplementación (diferencial).
- **Hooks por contexto**: capturar el input exacto de una primitiva en sus
  call-sites (md5-update, hex-encode) con correlación por hilo/ctx, en vez de
  adivinar formatos.
- **RTTI + logs**: la librería conserva nombres de clase/fichero y logs DEBUG
  compilados — buen mapa inicial.

### Offsets de referencia (file == vaddr, build 4.99.5)

| Qué | Offset |
| --- | --- |
| Puente JNI `Call` | `0x3c2bf8` |
| `SecHttpClient::Initialize` | `0x41c4a8` |
| MD5 `sign_o3` (transform) | `0x63d900` |
| MD5 dns/stats/p2p (transform) | `0x63e71c` |
| Wrapper X25519 (libsodium) | `0x698260` |
| `crypto_scarmult_curve25519` | `0x69b1e8` |
| MurmurHash3_x64_128 | `0x63c09c` |
| Ticket builder (`encrypt.io`) | `0x399c24` |
| Cookie builder control | `0x4254fc` |
| d-producer media (plaintext) | `0x58bb68` |
| `SlbV13Protocol` vtable | `0x8c10a8` |
| nghttp2 submit_request / mem_recv | `0x88b604` / `0x876d2c` |

## 8. Artefactos del laboratorio (host, no versionados)

- `/tmp/slbwork/SLB-PROTOCOL-SPEC.md` — spec detallada con ejemplos
  (contiene material de sesión, chmod 600).
- `/tmp/slbwork/emulate_transform.py`, `full.asm`, `decrypted-strings.txt`,
  `libranger-dec.so`.
- `/tmp/magstv-libs/libranger-jni.so` (binario), `/tmp/magstv-4.99.5.apk`.
- Fuentes jadx: `/tmp/magstv-fixed-jadx/sources`.
