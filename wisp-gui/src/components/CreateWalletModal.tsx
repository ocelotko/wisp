import { useState } from "react";
import Button from "./Button";
import { invoke } from "@tauri-apps/api/core";

interface Props {
  onClose: () => void;
  onSuccess: () => void;
}

export const CreateWalletModal = ({ onClose, onSuccess }: Props) => {
  const [step, setStep] = useState<"form" | "result">("form");
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [seed, setSeed] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  const handleCreate = async () => {
    setLoading(true);
    setError("");
    try {
      const generatedSeed = await invoke<string>("create_new_wallet", {
        name,
        password,
      });
      setSeed(generatedSeed);
      setStep("result");
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="fixed inset-0 z-100 flex items-center justify-center bg-black/60 backdrop-blur-sm p-4">
      <div className="w-full max-w-lg bg-dark-surfaceContainerHigh rounded-4xl border border-dark-outlineVariant p-8 shadow-2xl animate-in fade-in zoom-in duration-300">
        {step === "form" ? (
          <>
            <h3 className="text-2xl font-bold mb-2">Create New Wallet</h3>
            <p className="text-dark-onSurfaceVariant mb-8">
              Set a name and a strong password to encrypt your local keys.
            </p>

            <div className="space-y-4">
              <div>
                <label className="text-xs font-bold text-dark-outline uppercase ml-1">
                  Wallet Name
                </label>
                <input
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  className="w-full mt-1 bg-dark-surfaceContainerLow border border-dark-outlineVariant rounded-xl px-4 py-3 text-dark-onSurface focus:outline-none focus:border-dark-primary transition-colors"
                  placeholder="My Secure Wallet"
                />
              </div>
              <div>
                <label className="text-xs font-bold text-dark-outline uppercase ml-1">
                  Password
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
                onClick={handleCreate}
                disabled={loading || !name || !password}
              >
                {loading ? "Generating..." : "Generate Wallet"}
              </Button>
            </div>
          </>
        ) : (
          <>
            <div className="size-12 bg-dark-primary/20 text-dark-primary rounded-full flex items-center justify-center mb-6">
              <svg
                xmlns="http://www.w3.org/2000/svg"
                fill="none"
                viewBox="0 0 24 24"
                strokeWidth={2.5}
                stroke="currentColor"
                className="size-6"
              >
                <path
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  d="M9 12.75 11.25 15 15 9.75M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0Z"
                />
              </svg>
            </div>
            <h3 className="text-2xl font-bold mb-2">Save your seed phrase</h3>
            <p className="text-dark-onSurfaceVariant mb-6">
              Write these 24 words down in order. If you lose them, your WISP is
              gone forever.
            </p>

            <div className="grid grid-cols-3 gap-2 bg-dark-surfaceContainerLow p-4 rounded-2xl border border-dark-outlineVariant mb-8">
              {seed.split(" ").map((word, i) => (
                <div key={i} className="text-sm">
                  <span className="text-dark-outline mr-2 text-[10px] font-bold">
                    {i + 1}
                  </span>
                  <span className="text-dark-onSurface font-medium">
                    {word}
                  </span>
                </div>
              ))}
            </div>

            <Button
              variant="primary"
              className="w-full"
              onClick={() => {
                onSuccess();
                onClose();
              }}
            >
              I've written it down
            </Button>
          </>
        )}
      </div>
    </div>
  );
};
