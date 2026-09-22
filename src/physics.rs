//! Backend físico de Starfighter sobre `rapier2d` (ADR-0013).
//!
//! Rapier se usa directamente, sin `bevy_rapier2d`: las reglas siguen en
//! Bevy ECS y este módulo es el único que conoce tipos de Rapier. El ECS es
//! la fuente de las velocidades y fuerzas que deciden las reglas; Rapier es
//! la fuente de posiciones y rotaciones. Cada tick se sincroniza en ese
//! orden: ECS -> Rapier, un único `step`, Rapier -> ECS.
//!
//! Garantías de orden (Rapier solo es determinista si el orden de altas,
//! bajas y escrituras es idéntico):
//! - los cuerpos nuevos se insertan ordenados por `EntityId`;
//! - las bajas ocurren al aplicar los `Commands` del tick, en su orden;
//! - los cuerpos nunca duermen, así que no hay estado de sueño oculto.
//!
//! Las balas no son cuerpos rígidos: cada tick se barren con `cast_shapes`
//! contra el movimiento real de cada objetivo durante el paso, y gana el
//! primer impacto. Así no atraviesan naves a ninguna velocidad y no empujan
//! a nadie.

use crate::{Bullet, CollisionType, EntityId, Fighter, Settings};
use bevy::prelude::*;
use rapier2d::parry::query::{cast_shapes, ShapeCastOptions};
use rapier2d::prelude as rp;

/// Radio del collider de una bala.
pub const BULLET_RADIUS: f32 = 3.0;
/// Escala de longitudes de Rapier: tamaño típico de un objeto del juego,
/// en unidades del mundo (el mismo valor que usaba Avian2D).
const LENGTH_UNIT: f32 = 50.0;

#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct LinearVelocity(pub Vec2);

impl LinearVelocity {
    pub const ZERO: Self = Self(Vec2::ZERO);
}

impl std::ops::Deref for LinearVelocity {
    type Target = Vec2;
    fn deref(&self) -> &Vec2 {
        &self.0
    }
}

impl std::ops::DerefMut for LinearVelocity {
    fn deref_mut(&mut self) -> &mut Vec2 {
        &mut self.0
    }
}

#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct AngularVelocity(pub f32);

impl AngularVelocity {
    pub const ZERO: Self = Self(0.0);
}

/// Fuerza que las reglas aplican durante el tick (propulsión o frenado).
/// Persiste entre ticks hasta que una acción la cambie.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct ThrustForce(pub Vec2);

#[derive(Clone, Debug, PartialEq)]
pub enum PhysicsShape {
    ConvexHull(Vec<Vec2>),
    Ball(f32),
}

/// Pide un cuerpo dinámico en Rapier para esta entidad. `physics_insert`
/// lo crea en el siguiente tick a partir de su `Transform` y velocidades.
#[derive(Component, Clone, Debug)]
pub struct PhysicsBody {
    pub shape: PhysicsShape,
    pub lock_rotation: bool,
}

/// Handles de Rapier de una entidad que ya tiene cuerpo.
#[derive(Component, Clone, Copy, Debug)]
pub struct PhysicsHandle {
    pub body: rp::RigidBodyHandle,
    pub collider: rp::ColliderHandle,
}

#[derive(Resource)]
pub struct Physics {
    pub world: rp::PhysicsWorld,
}

impl Physics {
    pub fn new(settings: &Settings) -> Self {
        let mut world = rp::PhysicsWorld::new();
        world.gravity = rp::Vector::ZERO;
        world.integration_parameters.dt =
            settings.tick_rate.seconds_per_tick() as f32;
        world.integration_parameters.length_unit = LENGTH_UNIT;
        Self { world }
    }
}

/// Un contacto que las reglas deben resolver en este tick. `time_of_impact`
/// es la fracción del tick en que ocurre (1.0 para contactos detectados al
/// final del paso físico).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Contact {
    pub a: Entity,
    pub b: Entity,
    pub time_of_impact: f32,
}

#[derive(Resource, Default, Debug)]
pub struct TickContacts(pub Vec<Contact>);

/// `user_data` de un collider: `StableEntityId` en los 64 bits altos y
/// `Entity` de Bevy en los bajos.
fn pack_user_data(id: EntityId, entity: Entity) -> u128 {
    (u128::from(id.0 .0) << 64) | u128::from(entity.to_bits())
}

fn unpack_entity(user_data: u128) -> Option<Entity> {
    Entity::try_from_bits(user_data as u64)
}

pub fn unpack_stable_id(user_data: u128) -> u64 {
    (user_data >> 64) as u64
}

pub fn to_rapier(v: Vec2) -> rp::Vector {
    rp::Vector::new(v.x, v.y)
}

pub fn from_rapier(v: rp::Vector) -> Vec2 {
    Vec2::new(v.x, v.y)
}

/// Coseno y seno de una rotación sobre Z, sin funciones trigonométricas:
/// para q = (0, 0, sin(θ/2), cos(θ/2)), cos θ = w² - z² y sin θ = 2zw.
pub fn quat_to_cos_sin(q: Quat) -> (f32, f32) {
    (q.w * q.w - q.z * q.z, 2.0 * q.z * q.w)
}

/// Inversa de `quat_to_cos_sin` usando solo `sqrt`, que IEEE 754 exige
/// correctamente redondeada en toda plataforma.
pub fn cos_sin_to_quat(cos: f32, sin: f32) -> Quat {
    let w = ((1.0 + cos) * 0.5).max(0.0).sqrt();
    let z = ((1.0 - cos) * 0.5).max(0.0).sqrt();
    Quat::from_xyzw(0.0, 0.0, if sin < 0.0 { -z } else { z }, w)
}

fn rapier_pose(transform: &Transform) -> rp::Pose {
    let (cos, sin) = quat_to_cos_sin(transform.rotation);
    rp::Pose::from_parts(
        to_rapier(transform.translation.truncate()),
        rp::Rotation::from_cos_sin_unchecked(cos, sin),
    )
}

pub fn add_physics(app: &mut App, settings: &Settings) {
    app.insert_resource(Physics::new(settings))
        .init_resource::<TickContacts>()
        .add_observer(remove_physics_body);
}

fn remove_physics_body(
    removed: On<Remove, PhysicsHandle>,
    handles: Query<&PhysicsHandle>,
    mut physics: ResMut<Physics>,
) {
    if let Ok(handle) = handles.get(removed.entity) {
        physics.world.remove_body(handle.body);
    }
}

type PendingBodyQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static EntityId,
        &'static PhysicsBody,
        &'static Transform,
        &'static LinearVelocity,
        Option<&'static AngularVelocity>,
    ),
    Without<PhysicsHandle>,
>;

/// Crea en Rapier los cuerpos pedidos por `PhysicsBody`, en orden de
/// `EntityId` para que los handles internos no dependan del orden de
/// iteración de Bevy.
pub fn physics_insert(
    mut cmd: Commands,
    mut physics: ResMut<Physics>,
    pending: PendingBodyQuery,
) {
    let mut pending: Vec<_> = pending.iter().collect();
    pending.sort_by_key(|(_, id, ..)| **id);
    for (entity, id, body, transform, linear, angular) in pending {
        let mut builder = rp::RigidBodyBuilder::dynamic()
            .pose(rapier_pose(transform))
            .linvel(to_rapier(linear.0))
            .angvel(angular.map(|a| a.0).unwrap_or(0.0))
            .can_sleep(false);
        if body.lock_rotation {
            builder = builder.lock_rotations();
        }
        let collider = match &body.shape {
            PhysicsShape::Ball(radius) => rp::ColliderBuilder::ball(*radius),
            PhysicsShape::ConvexHull(points) => {
                let points: Vec<_> =
                    points.iter().copied().map(to_rapier).collect();
                rp::ColliderBuilder::convex_hull(&points)
                    .expect("PhysicsShape::ConvexHull must be a valid hull")
            }
        }
        .user_data(pack_user_data(*id, entity));
        let (body, collider) = physics.world.insert(builder, collider);
        cmd.entity(entity).insert(PhysicsHandle { body, collider });
    }
}

type BodyQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static EntityId,
        &'static PhysicsHandle,
        &'static CollisionType,
        Option<&'static Fighter>,
        &'static mut Transform,
        &'static mut LinearVelocity,
        Option<&'static mut AngularVelocity>,
        Option<&'static ThrustForce>,
    ),
>;

type BulletQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static EntityId,
        &'static Bullet,
        &'static mut Transform,
        &'static LinearVelocity,
    ),
    Without<PhysicsHandle>,
>;

/// Un objetivo posible de una bala, con su movimiento durante el paso.
struct SweepTarget {
    entity: Entity,
    owner: Option<usize>,
    collider: rp::ColliderHandle,
    start: rp::Pose,
    displacement: rp::Vector,
}

/// Avanza la física exactamente un tick y deja en `TickContacts` los
/// contactos que las reglas deben resolver.
pub fn physics_step(
    mut physics: ResMut<Physics>,
    mut contacts: ResMut<TickContacts>,
    mut bodies: BodyQuery,
    mut bullets: BulletQuery,
) {
    contacts.0.clear();
    let world = &mut physics.world;
    let dt = world.integration_parameters.dt;

    // ECS -> Rapier, y pose inicial de cada cuerpo para barrer balas.
    let mut targets: Vec<(EntityId, SweepTarget)> = Vec::new();
    for (entity, id, handle, kind, fighter, _, linear, angular, force) in
        &bodies
    {
        let Some(body) = world.bodies.get_mut(handle.body) else {
            continue;
        };
        body.set_linvel(to_rapier(linear.0), true);
        if let Some(angular) = angular {
            body.set_angvel(angular.0, true);
        }
        body.reset_forces(true);
        if let Some(force) = force {
            body.add_force(to_rapier(force.0), true);
        }
        if matches!(kind, CollisionType::Fighter | CollisionType::Asteroid) {
            targets.push((
                *id,
                SweepTarget {
                    entity,
                    owner: fighter.map(|f| f.player_id),
                    collider: handle.collider,
                    start: *body.position(),
                    displacement: rp::Vector::ZERO,
                },
            ));
        }
    }

    world.step();

    // Rapier -> ECS.
    for (_, _, handle, _, _, mut transform, mut linear, angular, _) in
        &mut bodies
    {
        let Some(body) = world.bodies.get(handle.body) else {
            continue;
        };
        let translation = from_rapier(body.translation());
        transform.translation = translation.extend(transform.translation.z);
        let rotation = body.rotation();
        transform.rotation = cos_sin_to_quat(rotation.cos(), rotation.sin());
        linear.0 = from_rapier(body.linvel());
        if let Some(mut angular) = angular {
            angular.0 = body.angvel();
        }
    }
    for (_, target) in &mut targets {
        if let Some(collider) = world.colliders.get(target.collider) {
            target.displacement =
                collider.position().translation - target.start.translation;
        }
    }
    targets.sort_by_key(|(id, _)| *id);

    // Contactos entre cuerpos al final del paso.
    for pair in world.narrow_phase.contact_pairs() {
        if !pair.has_any_active_contact() {
            continue;
        }
        let (Some(c1), Some(c2)) = (
            world.colliders.get(pair.collider1),
            world.colliders.get(pair.collider2),
        ) else {
            continue;
        };
        if let (Some(a), Some(b)) =
            (unpack_entity(c1.user_data), unpack_entity(c2.user_data))
        {
            contacts.0.push(Contact {
                a,
                b,
                time_of_impact: 1.0,
            });
        }
    }

    // Barrido de balas: primer impacto contra naves ajenas, asteroides u
    // otras balas, con el movimiento lineal de ambos durante el tick.
    let ball = rp::SharedShape::ball(BULLET_RADIUS);
    let mut moving: Vec<(EntityId, Entity, usize, rp::Pose, rp::Vector)> =
        bullets
            .iter()
            .map(|(entity, id, bullet, transform, velocity)| {
                (
                    *id,
                    entity,
                    bullet.player_id,
                    rp::Pose::from_translation(to_rapier(
                        transform.translation.truncate(),
                    )),
                    to_rapier(velocity.0 * dt),
                )
            })
            .collect();
    moving.sort_by_key(|(id, ..)| *id);
    let options = ShapeCastOptions::with_max_time_of_impact(1.0);
    for (index, (_, entity, owner, start, displacement)) in
        moving.iter().enumerate()
    {
        let mut best: Option<(f32, Entity)> = None;
        let mut consider = |toi: f32, other: Entity| {
            if best.is_none_or(|(best_toi, _)| toi < best_toi) {
                best = Some((toi, other));
            }
        };
        for (_, target) in &targets {
            if target.owner == Some(*owner) {
                continue;
            }
            let Some(collider) = world.colliders.get(target.collider) else {
                continue;
            };
            if let Ok(Some(hit)) = cast_shapes(
                start,
                *displacement,
                ball.as_ref(),
                &target.start,
                target.displacement,
                collider.shape(),
                options,
            ) {
                consider(hit.time_of_impact, target.entity);
            }
        }
        for (other_index, (_, other, _, other_start, other_displacement)) in
            moving.iter().enumerate()
        {
            if other_index == index {
                continue;
            }
            if let Ok(Some(hit)) = cast_shapes(
                start,
                *displacement,
                ball.as_ref(),
                other_start,
                *other_displacement,
                ball.as_ref(),
                options,
            ) {
                consider(hit.time_of_impact, *other);
            }
        }
        if let Some((time_of_impact, other)) = best {
            contacts.0.push(Contact {
                a: *entity,
                b: other,
                time_of_impact,
            });
        }
    }

    for (_, _, _, mut transform, velocity) in &mut bullets {
        transform.translation += (velocity.0 * dt).extend(0.0);
    }
}
