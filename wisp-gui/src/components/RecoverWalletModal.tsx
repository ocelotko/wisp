import { useState } from "react";
import Button from "./Button";
import { invoke } from "@tauri-apps/api/core";

interface Props {
  onClose: () => void;
  onSuccess: () => void;
}

export const RecoverWalletModal = ({ onClose, onSuccess }: Props) => {
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [seedPhrase, setSeedPhrase] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  const handleRecover = async () => {
    if (!name || !password || !seedPhrase) return;

    setLoading(true);
    setError("");

    try {
      // Pro Step: Sanitize input (remove extra spaces, newlines, and force lowercase)
      const sanitizedSeed = seedPhrase
        .trim()
        .toLowerCase()
        .replace(/\s+/g, " ");

      // Basic validation: Check word count (standard is usually 12 or 24)
      const wordCount = sanitizedSeed.split(" ").length;
      if (wordCount !== 12 && wordCount !== 24) {
        throw new Error(
          `Invalid phrase length: ${wordCount} words. Expected 12 or 24.`,
        );
      }

      await invoke("recover_wallet_command", {
        name,
        password,
        seedPhrase: sanitizedSeed,
      });

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
        <header className="mb-8">
          <h3 className="text-2xl font-bold mb-2">Recover Wallet</h3>
          <p className="text-dark-onSurfaceVariant text-sm">
            Import an existing wallet using your 12 or 24-word secret recovery
            phrase.
          </p>
        </header>

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
                placeholder="My Old Wallet"
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
              Secret Recovery Phrase
            </label>
            <textarea
              value={seedPhrase}
              onChange={(e) => setSeedPhrase(e.target.value)}
              placeholder="word1 word2 word3..."
              rows={4}
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
            disabled={loading || !name || !password || !seedPhrase}
          >
            {loading ? "Restoring..." : "Restore Wallet"}
          </Button>
        </div>
      </div>
    </div>
  );
};
