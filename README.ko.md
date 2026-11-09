# lD Mixer

리눅스용 **Audient iD** USB 오디오 인터페이스 믹서이자 컨트롤 패널입니다.
Audient의 공식 iD Mixer 앱을 본떠 만들었습니다. 이름은 말장난입니다:
**lD** = **L**inux **iD**.

[English README](README.md)

- Main, Cue A, Cue B 믹스, 모니터 섹션(볼륨, Dim, Cut, Alt, Talkback,
  Mono, 위상 반전), 출력 라우팅, 하드웨어 루프백, 미터, 프리셋.
- 실행 중에도 소리가 끊기지 않습니다. 커널 모듈이나 드라이버 교체가
  필요 없습니다.
- 다른 앱용 가상 입력·출력(예: Discord 전용 "DAW 3+4" 스피커)을 선택적으로
  만들 수 있고, lD Mixer가 실행 중일 때만 존재합니다.
- 트레이 아이콘이 있는 백그라운드 서비스로 동작해서, 창을 닫아도 믹스는
  그대로 유지됩니다.

## 프로젝트 소개

**목적.** lD Mixer는 리눅스에서 Audient의 iD Mixer를 대체하기 위해
만들어졌으며, 특정 기기가 아니라 Audient iD 시리즈 전반을 기준으로
설계되었습니다. 모델마다 프로필이 있고, 실제 하드웨어에서 검증되지 않은
모델은 인식만 하고 제어하지 않으므로 아무 명령도 보내지 않습니다.

**테스트한 환경** (그 외는 테스트되지 않음):

| | |
|---|---|
| 인터페이스 | Audient **iD24**, 펌웨어 1.12 |
| 배포판 | Artix Linux, 커널 7.3.1 (CachyOS) |
| 오디오 | PipeWire 1.6.11, WirePlumber 0.5.18, EasyEffects 8.3 |
| 데스크톱 | niri (Wayland), quickshell 트레이, adw-gtk3 테마 |
| 툴킷 | GTK 4.24, libadwaita 1.10, Rust 1.99 |

> [!WARNING]
> **AI로 제작되었고, 사람의 검수를 거치지 않았습니다.** lD Mixer는 AI의
> 도움을 받아 만들었으며(약 90% "바이브 코딩"), **제대로 된 사람의 코드
> 검수가 없었습니다.** 공식이 아닌, 리버스 엔지니어링한 명령으로 오디오
> 인터페이스와 통신합니다. **사용으로 인해 생기는 문제는 사용자 본인
> 부담입니다.** 하드웨어, 청력, 스피커 손상을 포함한 어떤 문제에도 제작자는
> 책임지지 않습니다. 새로운 기능을 시험할 때는 모니터 볼륨을 낮게 두세요.
> 이 프로젝트는 Audient와 관계가 없습니다.

---

## 목차

1. [프로젝트 소개](#프로젝트-소개)
2. [필요한 것](#필요한-것)
3. [설치](#설치)
4. [처음 실행하기](#처음-실행하기)
5. [lD Mixer가 시스템에서 바꾸는 것](#ld-mixer가-시스템에서-바꾸는-것)
6. [삭제](#삭제)
7. [사용법](#사용법)
8. [문제 해결](#문제-해결)
9. [개발자용](#개발자용)

---

## 필요한 것

| 항목 | 버전 | 용도 |
|------|------|------|
| Audient iD24 | | 제어할 인터페이스 |
| GTK 4와 libadwaita가 있는 리눅스 | GTK ≥ 4.18, libadwaita ≥ 1.7 | 창(GUI) |
| libusb | 1.0 | 인터페이스와 통신 |
| Rust (cargo) | ≥ 1.85 (edition 2024) | 빌드 |
| PipeWire와 명령줄 도구 | 최신 버전 | 가상 입력·출력과 루프백 입력에만 필요 |

최신 배포판(Arch 계열, Fedora 42 이상, NixOS unstable)은 대부분 조건을
만족합니다. 오래된 LTS 배포판은 libadwaita가 너무 오래되었을 수 있습니다.

---

## 설치

어떤 방법으로 설치하든 **같은 파일**이 설치되고
([바꾸는 것](#ld-mixer가-시스템에서-바꾸는-것) 참고), 명령어 하나로 지울 수
있습니다([삭제](#삭제) 참고).

### 1. 소스 받기

```sh
git clone https://github.com/lovebycoma/ldmixer.git
cd ldmixer
```

### 2. 배포판에 맞게 빌드하고 설치하기

**Arch Linux, Manjaro, EndeavourOS, CachyOS, Artix (makepkg)**

```sh
cd packaging/arch
makepkg -si
```

`-s`는 빌드에 필요한 도구(cargo, make 등)를 설치하고, `-i`는 완성된 패키지를
pacman으로 설치합니다. 패키지는 클론한 폴더의 커밋된 상태로 빌드됩니다.

**Fedora**

```sh
sudo dnf install cargo rust make rpm-build \
    'pkgconfig(gtk4)' 'pkgconfig(libadwaita-1)' 'pkgconfig(libusb-1.0)'
packaging/fedora/build-rpm.sh
sudo dnf install packaging/fedora/out/RPMS/*/ldmixer-*.rpm
```

**Nix / NixOS**

```sh
nix profile install .
```

NixOS에서는 USB 접근을 위해 설정에서 udev 규칙을 켜야 합니다:

```nix
imports = [ ldmixer.nixosModules.default ];   # ldmixer = 이 flake
programs.ldmixer.enable = true;
```

**그 외 배포판 (Debian, Ubuntu, openSUSE 등)**

Rust, make, pkg-config와 GTK 4, libadwaita, libusb 개발 파일을 설치한 뒤
(Debian/Ubuntu: `libgtk-4-dev libadwaita-1-dev libusb-1.0-0-dev`):

```sh
make
sudo make install            # /usr/local 아래에 설치됩니다
```

### 3. 인터페이스를 한 번 뽑았다 다시 꽂기

설치 과정에서 내 계정이 인터페이스를 열 수 있게 해 주는 규칙이 추가됩니다.
이 규칙은 설치 후에 연결된 장치에 적용되므로, iD24를 한 번 다시 꽂거나
재부팅하세요.

---

## 처음 실행하기

```sh
ldmixer gui
```

또는 앱 메뉴에서 **lD Mixer**를 여세요.

- 처음 실행할 때는 **인터페이스에 이미 있는 믹스를 읽어 옵니다**. 덮어쓰지
  않습니다. 그 뒤로는 인터페이스를 연결할 때마다 lD Mixer에 저장된 믹스를
  다시 적용합니다.
- 창을 닫아도 lD Mixer는 백그라운드(트레이 아이콘)에서 계속 실행됩니다.
  완전히 끄려면 트레이 메뉴의 **Quit**, 창 메뉴의 **Quit lD Mixer
  Completely**, 또는 `ldmixer stop`을 사용하세요.
- 입력과 루프백을 다른 앱에서 쓰려면 사운드 설정에서 인터페이스의 PipeWire
  프로필을 **Multichannel Duplex**(또는 **Pro Audio**)로 두세요. 대부분은
  이미 Duplex입니다.

장치가 없다면 `ldmixer gui --mock`으로 가상의 iD24를 사용해 UI 전체를
살펴볼 수 있습니다.

---

## lD Mixer가 시스템에서 바꾸는 것

lD Mixer는 **시스템 설정을 바꾸지 않도록** 만들어졌습니다. 아래가 추가하거나
건드리는 것의 전체 목록입니다.

### 패키지가 설치하는 파일 (시스템 전체)

| 경로 | 설명 |
|------|------|
| `/usr/bin/ldmixer` | 프로그램 (명령어는 이것 하나뿐) |
| `/usr/share/applications/com.lovebycoma.ldmixer.desktop` | 앱 메뉴 항목 |
| `/usr/share/icons/hicolor/scalable/apps/com.lovebycoma.ldmixer.svg` | 아이콘 |
| `/usr/share/metainfo/com.lovebycoma.ldmixer.metainfo.xml` | 소프트웨어 센터용 앱 설명 |
| `/usr/share/licenses/ldmixer/LICENSE` | 라이선스 |
| `/usr/share/ldmixer/manifest` | 설치된 파일 목록 (삭제 도구가 사용) |
| `/usr/lib/udev/rules.d/70-ldmixer.rules` | **시스템에 영향을 주는 유일한 규칙**, 아래 설명 참고 |

`sudo make install`로 설치하면 같은 파일이 `/usr/local` 아래에, udev 규칙은
`/etc/udev/rules.d/`에 설치됩니다.

**udev 규칙.** 리눅스는 기본적으로 root만 USB 장치를 열 수 있습니다. 이
한 줄짜리 규칙은 컴퓨터 앞에 로그인한 사용자에게 Audient(USB 제조사
`2708`) 장치에 대한 접근 권한만 줍니다. 다른 장치에는 영향이 없습니다:

```
SUBSYSTEM=="usb", ATTRS{idVendor}=="2708", TAG+="uaccess"
```

규칙이 설치되거나 삭제될 때 패키지 관리자가 udev를 다시 불러옵니다.

### 홈 폴더에 만드는 파일

| 경로 | 설명 |
|------|------|
| `~/.config/ldmixer/iD24.json` | 내 믹스 (페이더, 라우팅, 이름 등) |
| `~/.config/ldmixer/settings.json` | 앱 설정 (예: 루프백 입력) |
| `~/.config/ldmixer/presets/` | 저장한 프리셋 |
| `~/.local/state/ldmixer/daemon.log` | 백그라운드 서비스 로그 |

### lD Mixer가 실행 중일 때만 (종료하면 사라짐)

| 항목 | 설명 |
|------|------|
| 백그라운드 서비스 `ldmixer daemon` | USB 연결을 유지하고 트레이 아이콘을 표시 |
| `$XDG_RUNTIME_DIR/ldmixer.sock` | 창과 명령어가 서비스와 통신하는 로컬 소켓 (`/run/user/<uid>/`, 본인만 접근 가능) |
| 가상 장치 | 각각 `pw-loopback` 프로세스이며, 프로세스가 끝나면 장치도 사라집니다. **PipeWire나 WirePlumber 설정 파일은 쓰지 않습니다.** |

### 인터페이스 자체

lD Mixer는 Audient 앱과 마찬가지로 믹스, 라우팅, 모니터 설정을 USB로
인터페이스에 보냅니다. **인터페이스는 그 값을 다시 바뀔 때까지 기억하며**,
출력 라우팅 같은 일부는 전원을 껐다 켜도 유지됩니다. lD Mixer를 삭제해도
이 값들은 원래대로 돌아가지 않습니다. 삭제 전에 lD Mixer 기본값으로
되돌리려면 **메뉴 → Reset Mixer…**를 사용하세요(DAW 1+2만 재생됨).

### 절대 건드리지 않는 것

PipeWire, WirePlumber, ALSA 설정, 샘플레이트와 Quantum(표시만 함), 기본
입력·출력 장치, 커널 모듈, 사용자 그룹, 시스템 서비스.

---

## 삭제

어떤 방법으로 설치했든 명령어 하나로 지웁니다:

```sh
ldmixer uninstall
```

무엇을 지울지 먼저 보여 주고 확인을 받은 뒤 진행합니다(`-y`는 확인 생략,
`--keep-user-data`는 내 설정 유지).

**지우는 것**

1. 백그라운드 서비스를 멈춥니다. 이때 가상 장치도 모두 사라집니다.
2. lD Mixer 파일을 지웁니다: `~/.config/ldmixer/`,
   `~/.local/state/ldmixer/`, `~/.cache/ldmixer/`, 자동 시작 항목(있으면).
3. 설치에 쓴 방법으로 프로그램을 제거합니다:
   `pacman -Rns ldmixer`, `dnf remove ldmixer`,
   `nix profile remove ldmixer`, 또는 `make install`의 경우 manifest에 적힌
   파일들. udev 규칙도 함께 지워지고 udev를 다시 불러옵니다.

3단계에서 비밀번호를 묻습니다.

**남는 것 (그리고 지우는 법)**

| 남는 것 | 지우는 법 |
|---------|-----------|
| **인터페이스 안에** 저장된 설정 | 삭제 전에 **Reset Mixer** 실행, 또는 다른 도구로 다시 설정 |
| 클론한 `ldmixer` 폴더 | 직접 삭제: `rm -rf ldmixer` |
| `makepkg -s`나 `dnf install`로 설치한 빌드 도구 (cargo 등) | 필요 없다면 패키지 관리자로 제거 |
| NixOS의 `programs.ldmixer` 설정 줄 | 설정에서 지우고 다시 빌드 |

배포판별 명령어(`sudo pacman -Rns ldmixer`, `sudo dnf remove ldmixer`,
`nix profile remove ldmixer`, `sudo make uninstall`)도 동작하지만, 이 경우
`~/.config/ldmixer/`는 남습니다.

---

## 사용법

### 창

```
┌──────────────────────────── 채널 스트립 ────────────────────────────┬── Mixes ──┬─ Monitor ─┐
│ Mic 1 │ Mic 2 │ Digi 1+2 │ … │ DAW 1+2 │ DAW 3+4 │ DAW 5+6          │ Main Mix  │ 볼륨      │
│ Ø 🔗 🎤                                                             │ Cue A     │ DIM CUT … │
│ M S                                                                 │ Cue B     │ 출력      │
│ 팬                                                                  │ (노브)    │ 시스템    │
│ -12.0 dB                                                            │           │           │
│ 미터 │ 페이더                                                       │           │           │
└─────────────────────────────────────────────────────────────────────┴───────────┴───────────┘
```

- **Mixes.** Main, Cue A, Cue B 중 하나를 펼치면(또는 Ctrl+1/2/3) 스트립이
  그 믹스를 보여 줍니다. 펼친 믹스에는 마스터 레벨 노브와 그 믹스가 나가는
  출력이 표시되고, 연필 버튼으로 이름을 바꿀 수 있습니다.
- **스트립**, 위에서 아래로:
  - **Ø** 위상 반전(마이크 입력), **🔗** 두 채널을 스테레오로 묶기(Link),
    **🎤/🔈** 그 스트립을 다른 앱용 입력·출력으로 내보내기
  - **M / S** 현재 믹스에서 뮤트와 솔로
  - **팬**: 묶인 쌍은 채널마다 하나씩(묶으면 L100 / R100, 풀면 둘 다 가운데)
  - **레벨**과 **미터 + 페이더**
- **값 직접 입력.** dB나 팬 숫자를 클릭해서 입력하세요: `-12`, `-6.5 dB`,
  `-inf`, `C`, `L30`, `R45`.
- **스트립을 우클릭**하면 이름을 바꾸거나 숨길 수 있습니다. 숨긴 스트립은
  **메뉴 → Hidden Channels**에서 다시 보이게 할 수 있습니다. 숨겨도 소리는
  바뀌지 않습니다.
- **Monitor 열.** 모니터 볼륨과 버튼은 전면 패널과 연동됩니다. **Outputs**는
  각 출력에서 무엇을 재생할지(메인 믹스, 큐, DAW 쌍)와 **루프백**으로 무엇을
  보낼지 고릅니다. **System**은 장치, 샘플레이트, PipeWire Quantum을
  보여 주고 광 포트를 ADAT 또는 S/PDIF로 설정합니다.
- **프리셋**은 메인 메뉴에 있습니다.

### 다른 앱용 가상 입력·출력

PipeWire는 iD24를 16채널 출력 하나와 12채널 입력 하나로 보여 주는데, 대부분의
앱은 이를 채널별로 쓰지 못합니다. 스트립의 **🎤/🔈**를 누르면 lD Mixer가
실행되는 동안 별도의 장치를 추가합니다:

- **Mic / Digi 스트립**은 입력이 됩니다. 예: "iD24 Mic 1".
- **DAW 스트립**은 출력이 됩니다. 예: "iD24 DAW 3+4".
- 묶인 스트립은 스테레오 장치 하나, 풀린 스트립은 모노 장치가 됩니다.
- 이름을 바꾼 스트립은 그 이름을 씁니다("iD24 Discord").
- Outputs 섹션의 **Loopback as App Input**은 "iD24 Loopback"을 추가합니다.

각 앱에서 이 장치를 고르거나, 사운드 설정에서 시스템 기본 장치로 지정하세요.
lD Mixer가 기본 장치를 직접 바꾸지는 않습니다.

### 백그라운드 서비스와 트레이

```sh
ldmixer start      # 백그라운드 서비스 시작
ldmixer status     # 서비스, 인터페이스, 가상 장치 상태
ldmixer stop       # 서비스 종료 (가상 장치도 사라짐)
ldmixer daemon     # 서비스를 포그라운드로 실행 (예: 자동 시작 항목에서)
```

트레이 아이콘으로 창을 열 수 있고, Dim, Cut, Alt, Mono, Talkback, Quit
메뉴가 있습니다. 트레이가 있는 환경(KDE Plasma, 대부분의 패널, Waybar,
GNOME은 AppIndicator 확장)이 필요하며, 트레이가 없어도 나머지는 모두 동작합니다.

### 명령줄

`ldmixer help`로 전체 목록을 볼 수 있습니다. 확인용 명령어는 읽기만 합니다:
`scan`, `dump`, `snapshot <파일>`, `meters`, `watch`, `get`. 장치에 쓰는
명령어(`restore`, `route`, `cell`, `monitor`, `level`)는 `--write`가
필요합니다. 서비스가 실행 중이면 서비스를 거쳐 동작합니다.

---

## 문제 해결

| 증상 | 확인할 것 |
|------|-----------|
| "permission denied opening the iD24" | 설치 후 인터페이스를 한 번 다시 꽂으세요. NixOS는 `programs.ldmixer`를 켜세요. |
| "the iD24 is in use by another program" | 다른 lD Mixer나 BiD/Monix/MixiD가 장치를 쓰고 있습니다. `ldmixer status`, `ldmixer stop`. |
| 소리가 안 남 | 인터페이스 전면 볼륨 노브, 스트립 페이더와 믹스 마스터, **Outputs**에서 스피커가 맞는 믹스로 라우팅되었는지, 앱이 iD24(또는 고른 가상 장치)로 재생하는지. |
| 가상 입력에 오류가 표시됨 | 카드 프로필을 Multichannel Duplex 또는 Pro Audio로, `pw-loopback`과 `pw-dump`(PipeWire 도구)가 설치되어 있는지. |
| 트레이 아이콘이 없음 | 데스크톱에 트레이가 없습니다. `ldmixer gui`와 `ldmixer stop`을 사용하세요. |
| 그 외 | `~/.local/state/ldmixer/daemon.log`를 보거나 터미널에서 `ldmixer daemon`을 실행해 보세요. |

---

## 개발자용

```sh
cargo build                   # 디버그 빌드
cargo test                    # 유닛 테스트
cargo test --test vdev_live -- --ignored   # 실제 PipeWire 테스트 (iD 인터페이스 필요)
cargo run -- gui --mock       # 가상 iD24로 UI 실행
```

- [docs/PROTOCOL.md](docs/PROTOCOL.md): USB 프로토콜과 하드웨어에서 측정한
  내용 (영어).
- [AGENTS.md](AGENTS.md): 구조, 규칙, 프로젝트 원칙 (영어).

## 크레딧

프로토콜 지식은 다음 프로젝트를 바탕으로 합니다:
[MixiD](https://github.com/TheOnlyJoey/MixiD),
[BiD](https://github.com/baakhoff/BiD),
[Monix](https://github.com/sKuhLight/monix),
[audient-id24-dkms](https://github.com/Magnetar-OS/audient-id24-dkms).

## 라이선스

MIT, [LICENSE](LICENSE) 참고.
