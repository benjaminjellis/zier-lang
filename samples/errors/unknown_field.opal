;; Error: Point has no field `z`
(type Point (
  (:x ~ Int)
  (:y ~ Int)))

(let main {} 
  (let [p (Point 3 4)] (:z p)))

