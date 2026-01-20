interface WalletButtonProps {
  name: string;
  isActive: boolean;
  isUnlocked: boolean;
  onClick: () => void;
}

const LockOpenIcon = (
  <svg
    xmlns="http://www.w3.org/2000/svg"
    fill="none"
    viewBox="0 0 24 24"
    strokeWidth={1.5}
    stroke="currentColor"
    className="size-4"
  >
    <path
      strokeLinecap="round"
      strokeLinejoin="round"
      d="M13.5 10.5V6.75a4.5 4.5 0 1 1 9 0v3.75M3.75 21.75h10.5a2.25 2.25 0 0 0 2.25-2.25v-6.75a2.25 2.25 0 0 0-2.25-2.25H3.75a2.25 2.25 0 0 0-2.25 2.25v6.75a2.25 2.25 0 0 0 2.25 2.25Z"
    />
  </svg>
);

const LockClosedIcon = (
  <svg
    xmlns="http://www.w3.org/2000/svg"
    fill="none"
    viewBox="0 0 24 24"
    strokeWidth={1.5}
    stroke="currentColor"
    className="size-4"
  >
    <path
      strokeLinecap="round"
      strokeLinejoin="round"
      d="M16.5 10.5V6.75a4.5 4.5 0 1 0-9 0v3.75m-.75 11.25h10.5a2.25 2.25 0 0 0 2.25-2.25v-6.75a2.25 2.25 0 0 0-2.25-2.25H6.75a2.25 2.25 0 0 0-2.25 2.25v6.75a2.25 2.25 0 0 0 2.25 2.25Z"
    />
  </svg>
);

const WalletButton = ({
  name,
  isActive,
  isUnlocked,
  onClick,
}: WalletButtonProps) => (
  <button
    onClick={onClick}
    className={`w-full group mb-3 p-4 rounded-3xl border transition-all duration-200 ${
      isActive
        ? "bg-dark-primary text-dark-onPrimary border-transparent shadow-[0_8px_20px_rgba(0,0,0,0.3)] scale-[1.02]"
        : "bg-dark-surfaceContainer border-dark-outlineVariant text-dark-onSurfaceVariant hover:bg-dark-surfaceVariant"
    }`}
  >
    <div className="flex justify-between items-start">
      <div className="text-left">
        <p className="text-[10px] uppercase tracking-widest font-black mb-1 opacity-70">
          Account
        </p>
        <p className="text-lg font-bold truncate max-w-35">{name}</p>
      </div>
      <div
        className={`mt-1 p-1.5 rounded-xl ${isActive ? "bg-white/20" : "bg-dark-outline/10"}`}
      >
        {isUnlocked ? LockOpenIcon : LockClosedIcon}
      </div>
    </div>
  </button>
);

export default WalletButton;
