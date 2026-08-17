import { useEffect } from 'react'
import { motion, useMotionValue, useTransform, animate } from 'framer-motion'

/** A number that smoothly counts to its target whenever the value changes. */
export default function AnimatedNumber({
  value,
  format,
  className,
}: {
  value: number
  format?: (n: number) => string
  className?: string
}) {
  const mv = useMotionValue(value)
  const text = useTransform(mv, (v) => (format ? format(v) : Math.round(v).toLocaleString('en-US')))
  useEffect(() => {
    const controls = animate(mv, value, { duration: 0.55, ease: [0.22, 1, 0.36, 1] as [number, number, number, number] })
    return () => controls.stop()
  }, [value, mv])
  return <motion.span className={className}>{text}</motion.span>
}
