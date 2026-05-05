import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import Button from "./Button";
import { CopyToClipboard } from "react-copy-to-clipboard";
import React from "react";
import LoadingSpinner from "./LoadingSpinner";

interface AddressManagementModalProps {
  onClose: () => void;
  walletName: string;
}

interface AddressGroup {
  index: number;
  aurora: string;
  shadow: string;
  classic: string;
}

const XMarkIcon = () => (
  <svg
    xmlns="http://www.w3.org/2000/svg"
    fill="none"
    viewBox="0 0 24 24"
    strokeWidth={1.5}
    stroke="currentColor"
    className="h-6 w-6"
  >
    <path
      strokeLinecap="round"
      strokeLinejoin="round"
      d="M6 18 18 6M6 6l12 12"
    />
  </svg>
);

const CheckIcon = ({ className = "h-5 w-5" }: { className?: string }) => (
  <svg
    xmlns="http://www.w3.org/2000/svg"
    fill="none"
    viewBox="0 0 24 24"
    strokeWidth={2.5}
    stroke="currentColor"
    className={className}
  >
    <path
      strokeLinecap="round"
      strokeLinejoin="round"
      d="m4.5 12.75 6 6 9-13.5"
    />
  </svg>
);

const DocumentDuplicateIcon = ({
  className = "h-5 w-5",
}: {
  className?: string;
}) => (
  <svg
    xmlns="http://www.w3.org/2000/svg"
    fill="none"
    viewBox="0 0 24 24"
    strokeWidth={1.5}
    stroke="currentColor"
    className={className}
  >
    <path
      strokeLinecap="round"
      strokeLinejoin="round"
      d="M15.75 17.25v3.375c0 .621-.504 1.125-1.125 1.125h-9.75a1.125 1.125 0 0 1-1.125-1.125V7.875c0-.621.504-1.125 1.125-1.125H6.75a9.06 9.06 0 0 1 1.5.124m7.5 10.376h3.375c.621 0 1.125-.504 1.125-1.125V11.25c0-4.46-3.243-8.161-7.5-8.876a9.06 9.06 0 0 0-1.5-.124H9.375c-.621 0-1.125.504-1.125 1.125v3.5m7.5 10.375H9.375a1.125 1.125 0 0 1-1.125-1.125v-9.25m12 6.625v-1.875a3.375 3.375 0 0 0-3.375-3.375h-1.5a1.125 1.125 0 0 1-1.125-1.125v-1.5a3.375 3.375 0 0 0-3.375-3.375H9.75"
    />
  </svg>
);

export const AddressManagementModal: React.FC<AddressManagementModalProps> = ({
  onClose,
  walletName,
}) => {
  const [addresses, setAddresses] = useState<AddressGroup[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [password, setPassword] = useState("");
  const [isGenerating, setIsGenerating] = useState(false);
  const [copiedAddress, setCopiedAddress] = useState<string | null>(null);

  const fetchAddresses = async () => {
    setLoading(true);
    setError(null);
    try {
      const fetchedAddresses: any[] = await invoke("get_wallet_addresses");
      const grouped: AddressGroup[] = [];
      fetchedAddresses.forEach((item) => {
        const match = item.label.match(/Address #(\d+) \((\w+)\):/);
        if (match) {
          const index = parseInt(match[1]) - 1;
          const type = match[2].toLowerCase();
          if (!grouped[index]) {
            grouped[index] = {
              index: index + 1,
              aurora: "",
              shadow: "",
              classic: "",
            };
          }
          (grouped[index] as any)[type] = item.address;
        }
      });
      setAddresses(grouped.filter(Boolean));
    } catch (err: any) {
      setError(err.toString());
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    fetchAddresses();
  }, []);

  const handleGenerateNewAddress = async () => {
    if (!password) {
      setError("Please enter your wallet password.");
      return;
    }
    setIsGenerating(true);
    setError(null);
    try {
      await invoke("generate_new_address_command", { password });
      setPassword("");
      await fetchAddresses();
    } catch (err: any) {
      setError(err.toString());
    } finally {
      setIsGenerating(false);
    }
  };

  const handleCopy = (address: string) => {
    setCopiedAddress(address);
    setTimeout(() => setCopiedAddress(null), 2000);
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center p-4">
      <div
        className="bg-dark-surfaceContainerHigh rounded-3xl shadow-2xl w-full max-w-2xl max-h-[90vh] flex flex-col"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex justify-between items-center p-6 border-b border-dark-outlineVariant">
          <h2 className="text-xl font-bold text-dark-onSurface">
            Manage Addresses for {walletName}
          </h2>
          <button
            onClick={onClose}
            className="text-dark-onSurface focus:outline-none"
          >
            <XMarkIcon />
          </button>
        </div>

        <div className="p-6 flex-1 overflow-y-auto">
          {loading ? (
            <div className="flex justify-center items-center h-48">
              <LoadingSpinner />
            </div>
          ) : error ? (
            <div className="text-red-500 text-center">{error}</div>
          ) : (
            <div className="space-y-6">
              {addresses.map((group) => (
                <div
                  key={group.index}
                  className="bg-dark-surfaceContainer rounded-xl p-4 border border-dark-outlineVariant"
                >
                  <h3 className="text-lg font-semibold mb-3 text-dark-primary">
                    Address #{group.index}
                  </h3>
                  <div className="space-y-2">
                    {group.aurora && (
                      <div className="flex items-center justify-between text-sm">
                        <span className="font-medium text-dark-onSurfaceVariant">
                          Aurora:
                        </span>
                        <div className="flex items-center gap-2">
                          <span className="font-mono text-dark-onSurface break-all">
                            {group.aurora}
                          </span>
                          <CopyToClipboard
                            text={group.aurora}
                            onCopy={() => handleCopy(group.aurora)}
                          >
                            <button className="text-dark-primary hover:text-dark-primaryContainer focus:outline-none flex items-center justify-center">
                              {copiedAddress === group.aurora ? (
                                <CheckIcon />
                              ) : (
                                <DocumentDuplicateIcon className="h-5 w-5" />
                              )}
                            </button>
                          </CopyToClipboard>
                        </div>
                      </div>
                    )}
                    {group.shadow && (
                      <div className="flex items-center justify-between text-sm">
                        <span className="font-medium text-dark-onSurfaceVariant">
                          Shadow:
                        </span>
                        <div className="flex items-center gap-2">
                          <span className="font-mono text-dark-onSurface break-all">
                            {group.shadow}
                          </span>
                          <CopyToClipboard
                            text={group.shadow}
                            onCopy={() => handleCopy(group.shadow)}
                          >
                            <button className="text-dark-primary hover:text-dark-primaryContainer focus:outline-none flex items-center justify-center">
                              {copiedAddress === group.shadow ? (
                                <CheckIcon />
                              ) : (
                                <DocumentDuplicateIcon className="h-5 w-5" />
                              )}
                            </button>
                          </CopyToClipboard>
                        </div>
                      </div>
                    )}
                    {group.classic && (
                      <div className="flex items-center justify-between text-sm">
                        <span className="font-medium text-dark-onSurfaceVariant">
                          Classic:
                        </span>
                        <div className="flex items-center gap-2">
                          <span className="font-mono text-dark-onSurface break-all">
                            {group.classic}
                          </span>
                          <CopyToClipboard
                            text={group.classic}
                            onCopy={() => handleCopy(group.classic)}
                          >
                            <button className="text-dark-primary hover:text-dark-primaryContainer focus:outline-none flex items-center justify-center">
                              {copiedAddress === group.classic ? (
                                <CheckIcon />
                              ) : (
                                <DocumentDuplicateIcon className="h-5 w-5" />
                              )}
                            </button>
                          </CopyToClipboard>
                        </div>
                      </div>
                    )}
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>

        <div className="p-6 border-t border-dark-outlineVariant flex flex-col gap-4">
          <input
            type="password"
            placeholder="Wallet Password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            className="w-full bg-dark-surfaceContainerLow border border-dark-outlineVariant rounded-xl px-4 py-3 text-dark-onSurface focus:outline-none focus:border-dark-primary transition-colors"
          />
          <Button
            onClick={handleGenerateNewAddress}
            disabled={isGenerating || loading}
            className="w-full focus:outline-none"
          >
            {isGenerating ? <LoadingSpinner /> : "Generate New Address"}
          </Button>
        </div>
      </div>
    </div>
  );
};
