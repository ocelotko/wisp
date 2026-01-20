import { useState } from "react";
import Button from "./Button";
import { invoke } from "@tauri-apps/api/core";

interface Props {
  onClose: () => void;
  onSuccess: () => void;
}

type RecoveryMode = "seed" | "privateKey";

export const RecoverWalletModal = ({ onClose, onSuccess }: Props) => {
  const [recoveryMode, setRecoveryMode] = useState<RecoveryMode>("seed");
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [inputData, setInputData] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  const handleRecover = async () => {
    if (!name || !password || !inputData) return;

    setLoading(true);
    setError("");

    try {
      const sanitizedInput = inputData.trim();

      if (recoveryMode === "seed") {
        const seed = sanitizedInput.toLowerCase().replace(/\s+/g, " ");
        const wordCount = seed.split(" ").length;
        if (wordCount !== 12 && wordCount !== 24) {
          throw new Error(`Invalid seed phrase length: ${wordCount} words.`);
        }

        await invoke("recover_wallet_command", {
          name,
          password,
          seedPhrase: seed,
        });
      } else {
        const isHex = /^[0-9a-fA-F]+$/.test(sanitizedInput);
        if (!isHex) {
          throw new Error("Private key must be a valid hexadecimal string.");
        }
        if (sanitizedInput.length !== 64) {
          throw new Error(
            "Standard SECP256K1 private keys are 64 characters (32 bytes) long.",
          );
        }

        await invoke("recover_from_pk_command", {
          name,
          password,
          privateKey: sanitizedInput,
        });
      }

      onSuccess();
      onClose();
    } catch (err: any) {
      console.error("Recovery failed:", err);
      setError(err.message || String(err));
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="fixed inset-0 z-100 flex items-center justify-center bg-black/60 backdrop-blur-sm p-4">
      <div className="w-full max-w-xl bg-dark-surfaceContainerHigh rounded-4xl border border-dark-outlineVariant p-8 shadow-2xl animate-in fade-in zoom-in duration-300">
        <header className="mb-6">
          <h3 className="text-2xl font-bold mb-2">Recover Wallet</h3>
          <p className="text-dark-onSurfaceVariant text-sm">
            Import an existing account to the Wisp network.
          </p>
        </header>

        <div className="flex bg-dark-surfaceContainerLow p-1 rounded-xl mb-8 border border-dark-outlineVariant">
          <button
            onClick={() => setRecoveryMode("seed")}
            className={`flex-1 py-2 text-sm font-bold rounded-lg transition-all ${
              recoveryMode === "seed"
                ? "bg-dark-secondaryContainer text-dark-onSecondaryContainer shadow-sm"
                : "text-dark-outline hover:text-dark-onSurface"
            }`}
          >
            Seed Phrase
          </button>
          <button
            onClick={() => setRecoveryMode("privateKey")}
            className={`flex-1 py-2 text-sm font-bold rounded-lg transition-all ${
              recoveryMode === "privateKey"
                ? "bg-dark-secondaryContainer text-dark-onSecondaryContainer shadow-sm"
                : "text-dark-outline hover:text-dark-onSurface"
            }`}
          >
            Private Key
          </button>
        </div>

        <div className="space-y-6">
          <div className="grid grid-cols-2 gap-4">
            <div className="space-y-1.5">
              <label className="text-[10px] uppercase tracking-widest text-dark-outline font-bold ml-1">
                Wallet Name
              </label>
              <input
                type="text"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="My Wallet"
                className="w-full bg-dark-surfaceContainerLow border border-dark-outlineVariant rounded-xl px-4 py-3 text-dark-onSurface focus:outline-none focus:border-dark-primary transition-all"
              />
            </div>
            <div className="space-y-1.5">
              <label className="text-[10px] uppercase tracking-widest text-dark-outline font-bold ml-1">
                Password
              </label>
              <input
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                placeholder="••••••••"
                className="w-full bg-dark-surfaceContainerLow border border-dark-outlineVariant rounded-xl px-4 py-3 text-dark-onSurface focus:outline-none focus:border-dark-primary transition-all"
              />
            </div>
          </div>

          <div className="space-y-1.5">
            <label className="text-[10px] uppercase tracking-widest text-dark-outline font-bold ml-1">
              {recoveryMode === "seed"
                ? "Secret Recovery Phrase"
                : "Private Key (Hex/Base58)"}
            </label>
            <textarea
              value={inputData}
              onChange={(e) => setInputData(e.target.value)}
              placeholder={
                recoveryMode === "seed" ? "word1 word2..." : "e.g. 5K7..."
              }
              rows={recoveryMode === "seed" ? 4 : 2}
              className="w-full bg-dark-surfaceContainerLow border border-dark-outlineVariant rounded-2xl px-4 py-3 text-dark-onSurface focus:outline-none focus:border-dark-primary transition-all resize-none font-mono text-sm leading-relaxed"
            />
          </div>
        </div>

        {error && (
          <div className="mt-6 p-4 bg-red-400/10 border border-red-400/20 rounded-xl">
            <p className="text-red-400 text-xs font-medium">{error}</p>
          </div>
        )}

        <div className="flex justify-end gap-3 mt-10">
          <Button variant="ghost" onClick={onClose} disabled={loading}>
            Cancel
          </Button>
          <Button
            variant="primary"
            onClick={handleRecover}
            disabled={loading || !name || !password || !inputData}
          >
            {loading ? "Restoring..." : "Restore Wallet"}
          </Button>
        </div>
      </div>
    </div>
  );
};
