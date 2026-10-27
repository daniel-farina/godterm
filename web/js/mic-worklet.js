class Mic16k extends AudioWorkletProcessor {
  constructor() {
    super();
    this.ratio = sampleRate / 16000;
    this.pos = 0;
    this.out = new Int16Array(320);
    this.n = 0;
    this.prev = 0;
    this.calls = 0;
    this.empty = 0;
    this.peak = 0;
    this.port.postMessage({ hello: true, rate: sampleRate });
  }
  process(inputs) {
    if (++this.calls % 64 === 0) {
      this.port.postMessage({ calls: this.calls, empty: this.empty, peak: this.peak });
      this.peak = 0;
    }
    const ch = inputs[0];
    if (!ch || !ch.length) { this.empty++; return true; }
    const len = ch[0].length;
    for (let i = 0; i < len; i++) {
      let s = 0;
      for (let c = 0; c < ch.length; c++) s += ch[c][i];
      s /= ch.length;
      const m = s < 0 ? -s : s;
      if (m > this.peak) this.peak = m;

      while (this.pos <= 1) {
        const v = this.prev + (s - this.prev) * this.pos;
        this.out[this.n++] = Math.max(-1, Math.min(1, v)) * 32767;
        if (this.n === this.out.length) {
          this.port.postMessage(this.out.buffer, [this.out.buffer]);
          this.out = new Int16Array(320);
          this.n = 0;
        }
        this.pos += this.ratio;
      }
      this.pos -= 1;
      this.prev = s;
    }
    return true;
  }
}
registerProcessor('mic16k', Mic16k);
