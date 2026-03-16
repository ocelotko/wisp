import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import Button from "./Button";

interface Props {
  onClose: () => void;
  onSuccess: () => void;
  currentBalance: number; // in smallest unit
}

type FeeType = "Fixed" | "Percent";

export const SendModal = ({ onClose, onSuccess, currentBalance }: Props) => {
  const [recipient, setRecipient] = useState("");
  const [amount, setAmount] = useState("");
  const [isSendMax, setIsSendMax] = useState(false);
  const [password, setPassword] = useState("");

  const [feeType, setFeeType] = useState<FeeType>("Fixed");
  const [feeValue, setFeeValue] = useState("0.0001");

  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    if (feeType === "Fixed") {
      setFeeValue("0.0001");
    } else {
      setFeeValue("0.1"); // Default to 0.1%
    }
  }, [feeType]);

  const handleSend = async () => {
    setLoading(true);
    setError("");
    try {
      let feeValueRaw: number;
      if (feeType === "Fixed") {
        feeValueRaw = parseFloat(feeValue) * 100_000_000; // to smallest unit
      } else {
        // feeValue is a percentage, e.g. "0.1" for 0.1%.
        // The backend expects a value that it will multiply by amount and divide by 10,000.
        // So for 0.1%, we need to pass 10. (10/10000 = 0.001)
        feeValueRaw = parseFloat(feeValue) * 100;
      }

      await invoke("send_funds_command", {
        isSendMax,
        recipientPublicKeyStr: recipient,
        amountStr: isSendMax ? "0" : amount,
        feeType: feeType,
        feeValueRaw: Math.round(feeValueRaw),
        password,
      });
      onSuccess();
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  const handleAmountChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    setIsSendMax(false);
    setAmount(e.target.value);
  };

  const handleSendMax = () => {
    setIsSendMax(true);
    setAmount((currentBalance / 100_000_000).toString());
  };

  const isFormValid =
    recipient &&
    password &&
    (isSendMax || (amount && parseFloat(amount) > 0)) &&
    feeValue &&
    !isNaN(parseFloat(feeValue));

  return (
    <div className="fixed inset-0 z-100 flex items-center justify-center bg-black/60 backdrop-blur-sm p-4">
      <div className="w-full max-w-xl bg-dark-surfaceContainerHigh rounded-4xl border border-dark-outlineVariant p-8 shadow-2xl animate-in fade-in zoom-in duration-300">
        <h3 className="text-2xl font-bold mb-2">Send WISP</h3>
        <p className="text-dark-onSurfaceVariant mb-8">
          Enter the recipient's details and amount to transfer.
        </p>

        <div className="space-y-4">
          <div>
            <label className="text-xs font-bold text-dark-outline uppercase ml-1">
              Recipient Public Key
            </label>
            <input
              value={recipient}
              onChange={(e) => setRecipient(e.target.value)}
              className="w-full mt-1 bg-dark-surfaceContainerLow border border-dark-outlineVariant rounded-xl px-4 py-3 text-dark-onSurface focus:outline-none focus:border-dark-primary transition-colors font-mono text-sm"
              placeholder="02... or 03..."
            />
          </div>

          <div>
            <label className="text-xs font-bold text-dark-outline uppercase ml-1">
              Amount (WISP)
            </label>
            <div className="relative">
              <input
                type="number"
                value={
                  isSendMax
                    ? (currentBalance / 100_000_000).toLocaleString(undefined, {
                        maximumFractionDigits: 8,
                        useGrouping: false,
                      })
                    : amount
                }
                onChange={handleAmountChange}
                disabled={isSendMax}
                className="w-full mt-1 bg-dark-surfaceContainerLow border border-dark-outlineVariant rounded-xl px-4 py-3 text-dark-onSurface focus:outline-none focus:border-dark-primary transition-colors disabled:opacity-70"
                placeholder="0.00"
              />
              <button
                onClick={handleSendMax}
                className="absolute right-2 top-1/2 -translate-y-1/2 mt-0.5 bg-dark-secondaryContainer text-dark-onSecondaryContainer px-3 py-1.5 rounded-lg text-xs font-bold hover:bg-opacity-80 transition-all"
              >
                Send Max
              </button>
            </div>
          </div>

          <div className="flex gap-4">
            <div className="flex-1">
              <label className="text-xs font-bold text-dark-outline uppercase ml-1">
                Fee Type
              </label>
              <div className="flex bg-dark-surfaceContainerLow p-1 rounded-xl mt-1 border border-dark-outlineVariant">
                <button
                  type="button"
                  onClick={() => setFeeType("Fixed")}
                  className={`flex-1 py-2 text-sm font-bold rounded-lg transition-all ${
                    feeType === "Fixed"
                      ? "bg-dark-secondaryContainer text-dark-onSecondaryContainer shadow-sm"
                      : "text-dark-outline hover:text-dark-onSurface"
                  }`}
                >
                  Fixed
                </button>
                <button
                  type="button"
                  onClick={() => setFeeType("Percent")}
                  className={`flex-1 py-2 text-sm font-bold rounded-lg transition-all ${
                    feeType === "Percent"
                      ? "bg-dark-secondaryContainer text-dark-onSecondaryContainer shadow-sm"
                      : "text-dark-outline hover:text-dark-onSurface"
                  }`}
                >
                  %
                </button>
              </div>
            </div>
            <div className="flex-1">
              <label className="text-xs font-bold text-dark-outline uppercase ml-1">
                {feeType === "Fixed" ? "Fee (WISP)" : "Fee (%)"}
              </label>
              <input
                type="number"
                value={feeValue}
                onChange={(e) => setFeeValue(e.target.value)}
                className="w-full mt-1 bg-dark-surfaceContainerLow border border-dark-outlineVariant rounded-xl px-4 py-3 text-dark-onSurface focus:outline-none focus:border-dark-primary transition-colors"
                placeholder={feeType === "Fixed" ? "0.0001" : "0.1"}
              />
            </div>
          </div>

          <div>
            <label className="text-xs font-bold text-dark-outline uppercase ml-1">
              Wallet Password
            </label>
            <input
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              className="w-full mt-1 bg-dark-surfaceContainerLow border border-dark-outlineVariant rounded-xl px-4 py-3 text-dark-onSurface focus:outline-none focus:border-dark-primary transition-colors"
              placeholder="••••••••"
            />
          </div>
        </div>

        {error && (
          <p className="mt-4 text-red-400 text-sm font-medium">
            Error: {error}
          </p>
        )}

        <div className="flex justify-end gap-3 mt-10">
          <Button variant="ghost" onClick={onClose} disabled={loading}>
            Cancel
          </Button>
          <Button
            variant="primary"
            onClick={handleSend}
            disabled={loading || !isFormValid}
          >
            {loading ? "Sending..." : "Confirm & Send"}
          </Button>
        </div>
      </div>
    </div>
  );
};
