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
    const baseClassName =
      "py-3 px-6 rounded-full shadow-md text-center font-medium select-none cursor-pointer transition-all duration-300 flex items-center justify-center gap-2 active:scale-95 disabled:opacity-50 disabled:cursor-not-allowed " +
      className;

    let variantClassName = "";
    switch (variant) {
      case "primary":
        variantClassName = bgColorClassName
          ? "bg-dark-primary text-dark-onPrimary"
          : "bg-dark-primary hover:bg-primary-70 text-dark-onPrimary shadow-dark-primary/20";
        break;
      case "secondary":
        variantClassName = bgColorClassName
          ? "bg-dark-surfaceContainerHigh text-dark-onSurface"
          : "bg-dark-surfaceContainerHigh hover:bg-dark-surfaceContainerHighest text-dark-onSurface";
        break;
      case "danger":
        variantClassName = bgColorClassName
          ? "bg-dark-error text-dark-onError"
          : "bg-dark-error hover:bg-dark-errorContainer text-dark-onError";
        break;
      case "ghost":
        variantClassName = bgColorClassName
          ? "text-dark-primary bg-transparent border-2 border-dark-outline"
          : "text-dark-primary bg-transparent border-2 border-dark-outline hover:bg-primary-15/10";
        break;
      default:
        variantClassName = bgColorClassName
          ? "bg-dark-primary text-dark-onPrimary"
          : "bg-dark-primary hover:bg-primary-70 text-dark-onPrimary";
        break;
    }

    const finalClassName = `${baseClassName} ${variantClassName} ${
      bgColorClassName || ""
    } ${textColorClassName || ""}`;

    const buttonContent = (
      <>
        {!iconAfter && icon && icon} <span>{children}</span>
        {iconAfter && icon && icon}
      </>
    );

    if (href && !disabled) {
      return (
        <a href={href} className={finalClassName}>
          {buttonContent}
        </a>
      );
    }

    if (onClick || type) {
      return (
        <button type={type} className={finalClassName} onClick={onClick} disabled={disabled}>
          {buttonContent}
        </button>
      );
    }

    return (
      <div className={finalClassName}>
        {buttonContent}
      </div>
    );
  },
);

export default Button;
