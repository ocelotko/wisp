import { memo, ReactNode } from "react";

interface IconButtonProps {
  onClick?: () => void;
  icon: ReactNode;
  className?: string;
  disabled?: boolean;
}

const IconButton = memo(
  ({ onClick, icon, className = "", disabled = false }: IconButtonProps) => {
    return (
      <button
        onClick={onClick}
        disabled={disabled}
        className={`
        flex items-center justify-center rounded-full w-10 h-10 
        transition-colors duration-200 select-none
        text-dark-outline hover:text-dark-primary hover:bg-dark-primary/10 
        active:scale-90 disabled:opacity-50 disabled:cursor-not-allowed
        ${className}
      `}
      >
        <div className="size-6">{icon}</div>
      </button>
    );
  },
);

export default IconButton;
