"""Krill 알림음 krill-chime.caf를 합성한다(자체 제작, 외부 샘플 없음).

G5 → C6 두 음(완전4도 위로) 사인파 + 약한 2배음, 짧은 어택과 지수 감쇠, 전체 0.38초.
macOS 전용(afconvert). 실행: python3 packaging/macos/make_chime.py
bundle.sh가 결과를 Krill.app/Contents/Resources/에 넣고, 앱은 `NSSound soundNamed:@"krill-chime"`로 재생한다.
"""

import math
import pathlib
import struct
import subprocess
import tempfile
import wave

HERE = pathlib.Path(__file__).resolve().parent
OUT = HERE / "krill-chime.caf"
RATE = 44100
LENGTH = 0.38  # 초
NOTES = ((0.0, 783.99), (0.11, 1046.50))  # (시작 초, Hz)
PEAK = 0.45  # 최대 진폭(풀스케일 대비)


def sample(t):
    total = 0.0
    for start, freq in NOTES:
        dt = t - start
        if dt < 0:
            continue
        attack = min(dt / 0.006, 1.0)
        decay = math.exp(-dt / 0.09)
        phase = 2 * math.pi * freq * dt
        total += attack * decay * (math.sin(phase) + 0.18 * math.sin(2 * phase))
    # 끝 30ms를 0으로 줄여 잘림 클릭을 막는다.
    tail = min((LENGTH - t) / 0.03, 1.0)
    return total * tail


def main():
    n = int(RATE * LENGTH)
    raw = [sample(i / RATE) for i in range(n)]
    scale = PEAK / max(abs(x) for x in raw)
    frames = b"".join(struct.pack("<h", round(x * scale * 32767)) for x in raw)
    with tempfile.TemporaryDirectory() as tmp:
        wav = pathlib.Path(tmp) / "chime.wav"
        with wave.open(str(wav), "wb") as w:
            w.setnchannels(1)
            w.setsampwidth(2)
            w.setframerate(RATE)
            w.writeframes(frames)
        subprocess.run(["afconvert", "-f", "caff", "-d", "LEI16", str(wav), str(OUT)], check=True)
    print(f"wrote {OUT}")


if __name__ == "__main__":
    main()
