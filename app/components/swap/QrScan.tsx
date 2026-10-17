"use client";

import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui";

/** The browser's own QR decoder (Chromium, Android), when it has one; nothing is loaded from elsewhere. */
interface BarcodeDetectorLike {
  detect(source: CanvasImageSource | ImageBitmap): Promise<{ rawValue: string }[]>;
}
type BarcodeDetectorCtor = new (options: { formats: string[] }) => BarcodeDetectorLike;

function detectorCtor(): BarcodeDetectorCtor | null {
  if (typeof window === "undefined") return null;
  return (window as unknown as { BarcodeDetector?: BarcodeDetectorCtor }).BarcodeDetector ?? null;
}

/** "zcash:<address>?amount=…" (ZIP 321) or a bare address → the address. */
export function addressFromQr(text: string): string {
  const t = text.trim();
  const m = /^zcash:([a-z0-9]+)/i.exec(t);
  return (m ? m[1]! : t).toLowerCase();
}

/**
 * Reads an Ironwood address from a QR code with the camera, or from a picture of one, using the
 * browser's built-in decoder only. Where the browser has none, nothing is shown: paste the address.
 */
export function QrScan({ onAddress }: { onAddress: (address: string) => void }) {
  const Ctor = detectorCtor();
  const video = useRef<HTMLVideoElement | null>(null);
  const [scanning, setScanning] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!scanning || !Ctor) return;
    let stream: MediaStream | null = null;
    let stop = false;
    const detector = new Ctor({ formats: ["qr_code"] });
    void (async () => {
      try {
        stream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: "environment" } });
        if (stop || !video.current) return;
        video.current.srcObject = stream;
        await video.current.play();
        while (!stop && video.current) {
          const found = await detector.detect(video.current).catch(() => []);
          if (found[0]?.rawValue) {
            onAddress(addressFromQr(found[0].rawValue));
            setScanning(false);
            return;
          }
          await new Promise((r) => setTimeout(r, 300));
        }
      } catch {
        setError("The camera is not available. Paste the address, or choose a picture of the QR code.");
        setScanning(false);
      }
    })();
    return () => {
      stop = true;
      stream?.getTracks().forEach((t) => t.stop());
    };
  }, [scanning, Ctor, onAddress]);

  if (!Ctor) return null;
  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center gap-2">
        <Button variant="ghost" onClick={() => setScanning((s) => !s)}>
          {scanning ? "Stop the camera" : "Scan a QR code"}
        </Button>
        <label className="text-sm text-muted">
          <span className="sr-only">Picture of a QR code</span>
          <input
            type="file"
            accept="image/*"
            onChange={async (e) => {
              const f = e.target.files?.[0];
              e.target.value = "";
              if (!f) return;
              try {
                const found = await new Ctor({ formats: ["qr_code"] }).detect(await createImageBitmap(f));
                if (found[0]?.rawValue) onAddress(addressFromQr(found[0].rawValue));
                else setError("No QR code found in that picture.");
              } catch {
                setError("That picture could not be read.");
              }
            }}
            className="text-xs file:mr-2 file:rounded-md file:border file:border-border file:bg-surface-2 file:px-2 file:py-1 file:text-text"
          />
        </label>
      </div>
      {scanning ? <video ref={video} muted playsInline className="max-h-64 w-full max-w-sm rounded-lg bg-black" /> : null}
      {error ? <p className="text-xs text-warn">{error}</p> : null}
    </div>
  );
}
