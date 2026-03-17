import { memo, useState } from "react";
import Button from "./Button";
import IconButton from "./IconButton";
import { QRCodeSVG } from "qrcode.react";

interface Props {
  onClose: () => void;
  address: string | null;
}

const CopyIcon = (
  <svg
    xmlns="http://www.w3.org/2000/svg"
    fill="none"
    viewBox="0 0 24 24"
    strokeWidth={1.5}
    stroke="currentColor"
    className="size-5"
  >
    <path
      strokeLinecap="round"
      strokeLinejoin="round"
      d="M15.75 17.25v3.375c0 .621-.504 1.125-1.125 1.125h-9.75a1.125 1.125 0 0 1-1.125-1.125V7.875c0-.621.504-1.125 1.125-1.125H6.75a9.06 9.06 0 0 1 1.5.124m7.5 10.376h3.375c.621 0 1.125-.504 1.125-1.125V11.25c0-4.46-3.243-8.161-7.5-8.876a9.06 9.06 0 0 0-1.5-.124H9.375c-.621 0-1.125.504-1.125 1.125v3.5m7.5 10.375H9.375a1.125 1.125 0 0 1-1.125-1.125v-9.25m12 6.625v-1.875a3.375 3.375 0 0 0-3.375-3.375h-1.5a1.125 1.125 0 0 1-1.125-1.125v-1.5a3.375 3.375 0 0 0-3.375-3.375H9.75"
    />
  </svg>
);

const CheckIcon = (
  <svg
    xmlns="http://www.w3.org/2000/svg"
    fill="none"
    viewBox="0 0 24 24"
    strokeWidth={2.5}
    stroke="currentColor"
    className="size-5 text-green-400"
  >
    <path
      strokeLinecap="round"
      strokeLinejoin="round"
      d="m4.5 12.75 6 6 9-13.5"
    />
  </svg>
);

export const ReceiveModal = memo(({ onClose, address }: Props) => {
  const [copied, setCopied] = useState(false);

  const handleCopy = () => {
    if (address) {
      navigator.clipboard.writeText(address);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  };

  return (
    <div className="fixed inset-0 z-100 flex items-center justify-center bg-black/60 backdrop-blur-sm p-4">
      <div className="w-full max-w-md bg-dark-surfaceContainerHigh rounded-4xl border border-dark-outlineVariant p-8 shadow-2xl animate-in fade-in zoom-in duration-300">
        <h3 className="text-2xl font-bold mb-2">Receive WISP</h3>
        <p className="text-dark-onSurfaceVariant mb-8">
          Share your public address to receive funds.
        </p>

        <div className="bg-dark-surfaceContainer p-6 rounded-3xl flex flex-col items-center justify-center">
          <div className="w-48 h-48 bg-white rounded-2xl flex items-center justify-center mb-6">
            <QRCodeSVG
              value={address || ""}
              size={176}
              bgColor={"#FFFFFF"}
              fgColor={"#000000"}
            />
          </div>

          <div className="w-full bg-dark-surfaceContainerLow border border-dark-outlineVariant rounded-xl px-4 py-3 text-dark-onSurface flex items-center gap-2">
            <p className="flex-1 font-mono text-xs break-all">{address}</p>
            <IconButton
              icon={copied ? CheckIcon : CopyIcon}
              onClick={handleCopy}
            />
          </div>
        </div>

        <div className="flex justify-end gap-3 mt-10">
          <Button variant="primary" onClick={onClose}>
            Done
          </Button>
        </div>
      </div>
    </div>
  );
});
