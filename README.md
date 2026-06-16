# ZDL-Echo
[https://github.com/havokzero/ZDL-Echo](https://github.com/havokzero/ZDL-Echo)

**ZDL-Echo** is a professional-grade Telecom Research & Signaling Toolkit engineered for the generation and analysis of legacy telecommunications signaling protocols. Designed by Zero Day Labs, this tool provides a unified interface for testing DTMF (Dual-Tone Multi-Frequency), MF (Multi-Frequency R1), and SF (Single-Frequency 2600 Hz) trunk signaling.

---

## Visual Overview

| Audio Routing Menu | Operational Context   | Main Interface |
| :--- |:-----------------------------| :--- |
| ![Main UI](images/img.png) | ![Routing](images/img_1.png) | ![Operations](images/img_2.png) |

---

## Capabilities

* **Dual-Engine Architecture:** Simultaneous TX generation and RX detection using non-blocking, multi-threaded DSP pipelines separated by crossbeam channels.
* **Advanced Voice-Falsing Rejection:** Core DSP engine utilizes phase coherence, twist limits (max 8.0 dB), and strict energy dominance thresholds to reject background speech and noise.
* **Protocol Support:**
  * **DTMF:** Standard touch-tone generation and decoding.
  * **MF (R1):** Inter-office signaling including KP, ST, and digit sets.
  * **SF:** Single-frequency trunk supervision (2600 Hz TX). *(Note: RX detection for 2600 Hz is disabled by default in source to prevent voice-falsing; toggle `DETECT_SF` in `decoder.rs` to enable).*
* **Real-Time Visualization:** Native oscilloscope rendering with auto-scaling for live signal analysis.
* **Dynamic Audio Routing:** WASAPI-native hardware endpoint scanning, allowing for hot-swapping virtual audio cables at runtime without OS-level defaults.
* **Sequence Dialing:** Built-in dial string processor with automated millisecond timing gaps for reliable sequence transmission.

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

This project uses `cpal` for low-latency audio hardware access and `egui` for GPU-accelerated rendering. The build profile is heavily optimized for DSP math operations and binary size reduction.

```bash
# Clone the repository
git clone [https://github.com/havokzero/ZDL-Echo.git](https://github.com/havokzero/ZDL-Echo.git)
cd ZDL-Echo

# Compile for production
cargo build --release
```

The compiled executable will be generated at `target/release/ZDL-Echo.exe`.
*The application is linked to the Windows Subsystem natively, ensuring it launches as a standalone GUI without a background terminal console.*

---

## Technical Specifications

| Component | Technology |
| :--- | :--- |
| **DSP Core** | Optimized Goertzel Algorithm (2nd-Order IIR) |
| **Concurrency** | Crossbeam Channel-based messaging |
| **UI Framework** | Eframe / Egui |
| **Hardware IO** | CPAL (Cross-Platform Audio Library) |
| **Compiler Optimization** | LTO enabled, single codegen unit, stripped symbols |

---

## Legal & Compliance Disclaimer

**ZDL-Echo is provided strictly for educational and authorized research purposes only.** Unauthorized access to telecommunications infrastructure, interception of communications, or fraudulent use of signaling systems may be illegal. Ensure you have explicit authorization before connecting this software to any PSTN or radio network. By using this software, you agree to assume all liability for its operation.

---

*“Brought to you by Zero Day Labs.”*