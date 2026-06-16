# ZDL-Echo
[https://github.com/havokzero/ZDL-Echo](https://github.com/havokzero/ZDL-Echo)

**ZDL-Echo** is a professional-grade Telecom Research & Signaling Toolkit engineered for the generation and analysis of legacy telecommunications signaling protocols. Designed by Zero Day Labs, this tool provides a unified interface for testing DTMF (Dual-Tone Multi-Frequency), MF (Multi-Frequency R1), and SF (Single-Frequency 2600 Hz) trunk signaling.

---

## Capabilities

* **Dual-Engine Architecture:** Simultaneous TX generation and RX detection using non-blocking multi-threaded DSP pipelines.
* **Protocol Support:**
    * **DTMF:** Standard touch-tone generation/decoding.
    * **MF (R1):** Inter-office signaling including KP, ST, and digit sets.
    * **SF:** Single-frequency trunk supervision (2600 Hz).
* **Real-Time Visualization:** Native oscilloscope rendering for live signal analysis.
* **Dynamic Audio Routing:** WASAPI-native hardware endpoint scanning, allowing for hot-swapping virtual audio cables (Voicemeeter, Virtual Audio Cable, etc.) at runtime.
* **Sequence Dialing:** Built-in dial string processor with automated timing gaps for reliable sequence transmission.

---

## System Requirements

* **OS:** Windows 10/11
* **Audio Infrastructure:** **[Voicemeeter](https://vb-audio.com/Voicemeeter/)** is highly recommended for routing specific application audio into the ZDL-Echo RX capture stream.
* **Compiler:** Rust 1.80+ (Edition 2024)

---

## Setup & Audio Routing

To analyze signals from specific applications:

1. **Configure Virtual Audio:** Install Voicemeeter and set your target application's output to *Voicemeeter Input (Virtual Cable)*.
2. **Hook the Stream:** Launch ZDL-Echo, navigate to the **ROUTING** menu in the top bar, and select the corresponding *Voicemeeter Output* as your **RX CAPTURE SOURCE**.
3. **Monitor:** The `SIGNAL OSCILLOSCOPE` provides real-time waveform visualization, and the `TX/RX LOG` spools captured signaling data.

---

## Building from Source

This project uses `cpal` for low-latency audio hardware access and `egui` for GPU-accelerated rendering.

```bash
# Clone the repository
git clone https://github.com/havokzero/ZDL-Echo.git
cd ZDL-Echo

# Compile for production
cargo build --release
```

The optimized binary will be generated in `target/release/ZDL-Echo.exe`.

---

## Technical Specifications

| Component | Technology |
| :--- | :--- |
| **DSP Core** | Goertzel Algorithm (5th Order) |
| **Concurrency** | Crossbeam Channel-based messaging |
| **UI Framework** | Eframe / Egui |
| **Hardware IO** | CPAL (Cross-Platform Audio Library) |

---

## Legal & Compliance Disclaimer

**ZDL-Echo is provided strictly for educational and authorized research purposes only.** Unauthorized access to telecommunications infrastructure, interception of communications, or fraudulent use of signaling systems may be illegal. Ensure you have explicit authorization before connecting this software to any PSTN or radio network. By using this software, you agree to assume all liability for its operation.

---

*“Brought to you by Zero Day Labs.”*
