// The whole point of this example is the import below: `esptool-js/` is a git dependency
// that publishes only TypeScript, mounted from source (see ../src/main.rs) and compiled
// by the same pipeline that compiles this file. The specifier is a prefix into that
// mount — exactly how the compose example imports its siblings — and it resolves through
// the co-generated import map. Nothing here is aware that the package came from git.
import { ESPLoader, Transport } from 'esptool-js/src/index.js';

const ESPRESSIF_USB_VENDOR_ID = 0x303a;

const connectButton = document.getElementById('connect') as HTMLButtonElement;
const status = document.getElementById('status') as HTMLParagraphElement;
const report = document.getElementById('report') as HTMLElement;
const log = document.getElementById('log') as HTMLPreElement;

// esptool-js writes its connect trace through this interface.
const terminal = {
  clean: () => {
    log.textContent = '';
  },
  writeLine: (data: string) => {
    log.textContent += `${data}\n`;
  },
  write: (data: string) => {
    log.textContent += data;
  },
};

connectButton.addEventListener('click', () => {
  void describeChip();
});

if (!('serial' in navigator)) {
  status.textContent = 'This browser has no Web Serial. Use Chrome or Edge.';
  connectButton.disabled = true;
}

async function describeChip(): Promise<void> {
  connectButton.disabled = true;
  report.hidden = true;
  status.textContent = 'Connecting…';

  let transport: Transport | undefined;
  try {
    // Espressif's USB vendor id, so the picker offers boards rather than every port.
    const device = await navigator.serial.requestPort({
      filters: [{ usbVendorId: ESPRESSIF_USB_VENDOR_ID }],
    });
    transport = new Transport(device, true);
    const loader = new ESPLoader({ transport, baudrate: 115200, terminal });

    // Connect and identify only. `main()` would also upload the flasher stub and raise
    // the baud rate; this example never writes to a board, so it stops here.
    await loader.detectChip();

    const flashId = await loader.readFlashId();
    const rows: [string, string][] = [
      ['Chip', await loader.chip.getChipDescription(loader)],
      ['MAC', await loader.chip.readMac(loader)],
      ['Crystal', `${await loader.chip.getCrystalFreq(loader)} MHz`],
      ['Features', (await loader.chip.getChipFeatures(loader)).join(', ')],
      // Same decoding esptool-js's own `flashId()` prints: the size lives in byte 2.
      ['Flash', loader.DETECTED_FLASH_SIZES[(flashId >> 16) & 0xff] ?? 'unknown'],
    ];

    report.replaceChildren(
      ...rows.flatMap(([label, value]) => {
        const dt = document.createElement('dt');
        dt.textContent = label;
        const dd = document.createElement('dd');
        // Set as text: a chip can report whatever it likes, and it does not get to
        // inject markup into this page.
        dd.textContent = value;
        return [dt, dd];
      }),
    );
    report.hidden = false;
    status.textContent = 'Connected.';
  } catch (error) {
    // A cancelled port picker is a choice, not a fault.
    const message = error instanceof Error ? error.message : String(error);
    status.textContent = message.includes('No port selected')
      ? 'No board selected.'
      : `Could not read the chip: ${message}`;
  } finally {
    // Always release the port, so a second attempt can open it.
    await transport?.disconnect().catch(() => undefined);
    connectButton.disabled = false;
  }
}
