import { memo, ReactNode, JSX } from "react";

export type ButtonVariant = "primary" | "secondary" | "danger" | "ghost";

interface ButtonProps {
  onClick?: () => void;
  href?: string;
  variant?: ButtonVariant;
  children: ReactNode;
  className?: string;
  bgColorClassName?: string;
  textColorClassName?: string;
  icon?: JSX.Element;
  iconAfter?: boolean;
  disabled?: boolean;
  type?: "button" | "submit" | "reset";
}

const Button = memo(
  ({
    onClick,
    href,
    variant = "primary",
    children,
    className = "",
    bgColorClassName,
    textColorClassName,
    icon,
    iconAfter = false,
    disabled = false,
    type = "button",
  }: ButtonProps) => {
    // Unified base styles with WalletButton
    const baseClassName =
      "py-3.5 px-6 rounded-full font-bold select-none cursor-pointer transition-all duration-200 ease-in-out flex items-center justify-center gap-2 active:scale-[0.97] disabled:opacity-50 disabled:cursor-not-allowed disabled:active:scale-100 " +
      className;

    let variantClassName = "";
    switch (variant) {
      case "primary":
        variantClassName =
          "bg-dark-primary text-dark-onPrimary shadow-md shadow-dark-primary/20 hover:shadow-lg hover:bg-primary-70";
        break;
      case "secondary":
        variantClassName =
          "bg-dark-surfaceContainerHigh text-dark-onSurface hover:bg-dark-surfaceContainerHighest hover:shadow-md";
        break;
      case "danger":
        variantClassName =
          "bg-dark-error text-dark-onError shadow-md shadow-dark-error/20 hover:bg-dark-errorContainer";
        break;
      case "ghost":
        variantClassName =
          "text-dark-onSurface bg-transparent border border-dark-outlineVariant hover:bg-dark-surfaceVariant hover:border-dark-outline";
        break;
    }

    const finalClassName = `${baseClassName} ${variantClassName} ${bgColorClassName || ""} ${textColorClassName || ""}`;

    const buttonContent = (
      <>
        {!iconAfter && icon && <span className="shrink-0">{icon}</span>}
        <span className="leading-none">{children}</span>
        {iconAfter && icon && <span className="shrink-0">{icon}</span>}
      </>
    );

    const commonProps = {
      className: finalClassName,
      onClick: disabled ? undefined : onClick,
    };

    if (href && !disabled)
      return (
        <a href={href} {...commonProps}>
          {buttonContent}
        </a>
      );

    return (
      <button type={type} {...commonProps} disabled={disabled}>
        {buttonContent}
      </button>
    );
  },
);

export default Button;
